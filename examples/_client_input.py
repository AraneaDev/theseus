"""Private acquisition policy shared by the checkout's ELF example preparers."""
import os
import stat
import sys


def open_client(source):
    # Unix retains atomic leaf no-follow/nonblocking acquisition. Windows uses
    # checked lstat/open/lstat identities; this is not atomic no-follow or a
    # sandbox against same-user ancestor replacement.
    if os.name == 'nt' and sys.version_info < (3, 12):
        raise ValueError('Windows preparation requires Python 3.12+ native file identity')
    before = source.lstat()
    flags = os.O_RDONLY
    if os.name == 'nt':
        flags |= os.O_BINARY | os.O_NOINHERIT
    else:
        flags |= os.O_NOFOLLOW | os.O_NONBLOCK
    fd = os.open(source, flags)
    try:
        opened, after = os.fstat(fd), source.lstat()
        for info in [before, opened, after]:
            if not stat.S_ISREG(info.st_mode):
                raise ValueError('client must be a regular nonsymlink file')
            if os.name == 'nt':
                if not hasattr(info, 'st_file_attributes') or info.st_file_attributes & stat.FILE_ATTRIBUTE_REPARSE_POINT:
                    raise ValueError('client reparse attributes are missing or unsafe')
                if not info.st_ino or not info.st_dev:
                    raise ValueError('client native identity is unavailable')
        if not ((before.st_dev, before.st_ino) == (opened.st_dev, opened.st_ino) == (after.st_dev, after.st_ino)):
            raise ValueError('client identity changed during acquisition')
        stream = os.fdopen(fd, 'rb')
    except BaseException:
        os.close(fd)
        raise
    return stream
