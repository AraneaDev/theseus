//! Owning, aligned, bounded native output. Interior pointers are validated before
//! reading; native strings remain raw UTF-16 and MULTI_SZ has an explicit bound.
use super::model::{NATIVE_BUFFER_LIMIT, invalid};
use std::{
    io,
    mem::{align_of, size_of},
};
/// Types whose every initialized bit pattern is valid, including arbitrary
/// interior pointer values which are checked separately before dereference.
/// # Safety
/// Implementors must be Copy, repr(C)/plain integers, with no invalid bit patterns.
pub(super) unsafe trait NativeValue: Copy {}
unsafe impl NativeValue for u32 {}
unsafe impl NativeValue for i32 {}
unsafe impl NativeValue for u64 {}
unsafe impl<T: NativeValue, const N: usize> NativeValue for [T; N] {}

pub(super) struct NativeBuffer {
    words: Vec<usize>,
    bytes: usize,
}
impl NativeBuffer {
    pub fn new(bytes: usize) -> io::Result<Self> {
        if bytes == 0 || bytes > NATIVE_BUFFER_LIMIT {
            return Err(invalid("native SCM output size outside bound"));
        }
        Ok(Self {
            words: vec![0; bytes.div_ceil(size_of::<usize>())],
            bytes,
        })
    }
    pub fn ptr(&mut self) -> *mut u8 {
        self.words.as_mut_ptr().cast()
    }
    pub fn len(&self) -> u32 {
        self.bytes as u32
    }
    fn offset<T>(&self, ptr: *const T, count: usize) -> io::Result<usize> {
        let start = self.words.as_ptr() as usize;
        let addr = ptr as usize;
        let size = count
            .checked_mul(size_of::<T>())
            .ok_or_else(|| invalid("SCM native array overflow"))?;
        let offset = addr
            .checked_sub(start)
            .ok_or_else(|| invalid("SCM native pointer outside output"))?;
        if !addr.is_multiple_of(align_of::<T>())
            || offset.checked_add(size).is_none_or(|end| end > self.bytes)
        {
            return Err(invalid("SCM native pointer/length outside output"));
        }
        Ok(offset)
    }
    pub fn value<T: NativeValue>(&self) -> io::Result<T> {
        let p = self.words.as_ptr().cast::<T>();
        self.offset(p, 1)?;
        // SAFETY: initialized owning allocation, native type fits and is aligned;
        // callers use only integer/pointer repr(C) values with every bit pattern valid.
        Ok(unsafe { p.read() })
    }
    pub fn array<T: NativeValue>(&self, p: *const T, count: usize) -> io::Result<Vec<T>> {
        if count == 0 {
            return Ok(vec![]);
        }
        self.offset(p, count)?;
        // SAFETY: checked owning allocation and count; same repr(C) restriction.
        Ok(unsafe { std::slice::from_raw_parts(p, count) }.to_vec())
    }
    fn remaining_wide(&self, p: *const u16) -> io::Result<&[u16]> {
        let offset = self.offset(p, 1)?;
        // SAFETY: u16 accepts every bit pattern; bound and alignment checked.
        Ok(unsafe { std::slice::from_raw_parts(p, (self.bytes - offset) / 2) })
    }
    pub fn wide(&self, p: *const u16) -> io::Result<Vec<u16>> {
        if p.is_null() {
            return Ok(vec![]);
        }
        let units = self.remaining_wide(p)?;
        let end = units
            .iter()
            .position(|c| *c == 0)
            .ok_or_else(|| invalid("unterminated SCM wide string"))?;
        Ok(units[..end].to_vec())
    }
    pub fn multi(&self, p: *const u16) -> io::Result<Vec<Vec<u16>>> {
        if p.is_null() {
            return Ok(vec![]);
        }
        let mut units = self.remaining_wide(p)?;
        let mut result = vec![];
        if units.first() == Some(&0) {
            if units.get(1) == Some(&0) {
                return Ok(result);
            } else {
                return Err(invalid("unterminated empty SCM MULTI_SZ"));
            }
        }
        loop {
            let end = units
                .iter()
                .position(|c| *c == 0)
                .ok_or_else(|| invalid("unterminated SCM MULTI_SZ"))?;
            result.push(units[..end].to_vec());
            units = &units[end + 1..];
            if units.first() == Some(&0) {
                return Ok(result);
            }
            if result.len() > 64 {
                return Err(invalid("SCM MULTI_SZ member bound"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_wide_reads_refuse_external_unaligned_and_unterminated_pointers() {
        let mut b = NativeBuffer::new(32).unwrap();
        b.words[0] = 0x0062_0061;
        let ptr = b.ptr();
        assert_eq!(b.wide(ptr.cast()).unwrap(), vec![97, 98]);
        assert!(b.wide((ptr as usize + 1) as *const u16).is_err());
        assert!(b.wide((ptr as usize + 32) as *const u16).is_err());
        assert!(b.wide(std::ptr::dangling::<u16>()).is_err());
        b.words.fill(usize::MAX);
        assert!(b.wide(ptr.cast()).is_err());
    }
    #[test]
    fn multi_string_preserves_utf16_and_requires_bounded_double_nul() {
        let mut b = NativeBuffer::new(16).unwrap();
        let ptr = b.ptr();
        // Test byte population is not part of the production parser.
        unsafe {
            std::ptr::copy_nonoverlapping([0xd800u16, 0, 98, 0, 0].as_ptr(), ptr.cast(), 5);
        }
        assert_eq!(b.multi(ptr.cast()).unwrap(), vec![vec![0xd800], vec![98]]);
        b.words.fill(usize::MAX);
        assert!(b.multi(ptr.cast()).is_err());
    }
    #[test]
    fn native_buffer_bounds_refuse_oversize_structs_and_array_overflow() {
        assert!(NativeBuffer::new(NATIVE_BUFFER_LIMIT + 1).is_err());
        let mut b = NativeBuffer::new(8).unwrap();
        let ptr = b.ptr();
        assert_eq!(b.len(), 8);
        assert!(b.value::<[u64; 2]>().is_err());
        assert!(b.array::<u64>(ptr.cast(), usize::MAX).is_err());
        assert!(b.array::<u64>(std::ptr::null(), 0).unwrap().is_empty());
    }
}
