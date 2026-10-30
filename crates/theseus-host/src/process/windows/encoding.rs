//! CRT argv and Windows ordinal environment keys; no shell expansion.
use crate::process::command::invalid;
use crate::process::{ChildEnvironment, NativeCommand};
use std::cmp::Ordering;
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use windows_sys::Win32::Globalization::{
    CSTR_EQUAL, CSTR_GREATER_THAN, CSTR_LESS_THAN, CompareStringOrdinal,
};

pub(super) fn wide(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut value: Vec<u16> = value.encode_wide().collect();
    if value.contains(&0) {
        return Err(invalid("NUL in Windows string"));
    }
    value.push(0);
    Ok(value)
}

fn compare(a: &OsStr, b: &OsStr) -> io::Result<Ordering> {
    let a = wide(a)?;
    let b = wide(b)?;
    // NUL-terminated strings remain live; ordinal comparison is Windows' native
    // case-insensitive key ordering, including non-ASCII names.
    match unsafe { CompareStringOrdinal(a.as_ptr(), -1, b.as_ptr(), -1, 1) } {
        CSTR_LESS_THAN => Ok(Ordering::Less),
        CSTR_EQUAL => Ok(Ordering::Equal),
        CSTR_GREATER_THAN => Ok(Ordering::Greater),
        _ => Err(io::Error::last_os_error()),
    }
}

pub(super) fn environment(environment: &ChildEnvironment) -> io::Result<Option<Vec<u16>>> {
    let ChildEnvironment::Replace(entries) = environment else {
        return Ok(None);
    };
    let mut sorted: Vec<(&OsString, &OsString)> = Vec::new();
    for (key, value) in entries {
        let mut index = 0;
        while index < sorted.len() && compare(sorted[index].0, key)? == Ordering::Less {
            index += 1;
        }
        if index < sorted.len() && compare(sorted[index].0, key)? == Ordering::Equal {
            // std::Command preserves the first spelling and the last value.
            sorted[index].1 = value;
        } else {
            sorted.insert(index, (key, value));
        }
    }
    let mut block = Vec::new();
    for (key, value) in sorted {
        block.extend(key.encode_wide());
        block.push(b'=' as u16);
        block.extend(value.encode_wide());
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    Ok(Some(block))
}

pub(super) fn application(command: &NativeCommand) -> io::Result<PathBuf> {
    let program = Path::new(&command.program);
    let executable = |path: PathBuf| {
        if path.extension().is_none() {
            path.with_extension("exe")
        } else {
            path
        }
    };
    if program.components().count() > 1 || program.is_absolute() {
        let path = executable(std::path::absolute(program)?);
        return std::fs::canonicalize(path);
    }
    let path = match &command.environment {
        ChildEnvironment::Inherit => std::env::var_os("PATH"),
        ChildEnvironment::Replace(entries) => {
            let mut result = None;
            for (key, value) in entries {
                if compare(key, OsStr::new("PATH"))? == Ordering::Equal {
                    result = Some(value.clone());
                }
            }
            result
        }
    }
    .ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "native executable search requires PATH",
        )
    })?;
    for directory in std::env::split_paths(&path) {
        let candidate = executable(directory.join(program));
        if candidate.is_file() {
            return std::fs::canonicalize(candidate);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "native executable absent from declared PATH",
    ))
}

pub(super) fn command_line(application: &Path, args: &[OsString]) -> io::Result<Vec<u16>> {
    let mut output = vec![b'"' as u16];
    // argv[0] has special CRT parsing: executable filenames cannot contain quotes.
    let application = wide(application.as_os_str())?;
    output.extend_from_slice(&application[..application.len() - 1]);
    output.push(b'"' as u16);
    for arg in args {
        output.extend([b' ' as u16, b'"' as u16]);
        let mut slashes = 0;
        for unit in arg.encode_wide() {
            match unit {
                0 => return Err(invalid("NUL in argument")),
                92 => slashes += 1,
                34 => {
                    output.extend(std::iter::repeat_n(92, slashes * 2 + 1));
                    output.push(34);
                    slashes = 0;
                }
                _ => {
                    output.extend(std::iter::repeat_n(92, slashes));
                    output.push(unit);
                    slashes = 0;
                }
            }
        }
        output.extend(std::iter::repeat_n(92, slashes * 2));
        output.push(34);
    }
    output.push(0);
    if output.len() > 32767 {
        return Err(invalid("Windows command line exceeds 32767 UTF-16 units"));
    }
    Ok(output)
}
