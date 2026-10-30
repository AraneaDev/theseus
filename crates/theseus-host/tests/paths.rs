use std::io::ErrorKind;

#[test]
fn report_reference_cannot_change_roots() {
    for bad in [
        "", "../x", "/x", "a//b", "a/../b", "a/./b", "a\0b", "a/", ".", "..",
    ] {
        let error = theseus_host::fs::RelativePath::parse(bad).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidInput, "{bad:?}");
    }
    for good in ["logs/stdout", "x", "résumé/文.txt", ".hidden/file", "a..b"] {
        assert!(
            theseus_host::fs::RelativePath::parse(good).is_ok(),
            "{good:?}"
        );
    }
}

#[cfg(windows)]
#[test]
fn windows_aliases_are_not_report_components() {
    for bad in [
        "a:b",
        "C:/x",
        "a\\b",
        "NUL",
        "con.txt",
        "x.",
        "x ",
        "logs/AuX.txt",
        "PRN",
        "COM1",
        "com9.log",
        "LPT1",
        "lpt9.txt",
        "COM¹",
        "LPT².txt",
        "CONIN$",
        "CONOUT$",
        "x/CON.txt",
        "x\u{1f}",
        "a?b",
        "a*b",
        "a<b",
        "a>b",
        "a|b",
        "a\"b",
    ] {
        assert!(
            theseus_host::fs::RelativePath::parse(bad).is_err(),
            "{bad:?}"
        );
    }
    for good in ["COM0", "COM10", "LPT10", "console.txt", "x.y", "a b"] {
        assert!(
            theseus_host::fs::RelativePath::parse(good).is_ok(),
            "{good:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn unix_report_components_keep_native_names() {
    for good in ["a:b", "a\\b", "NUL", "con.txt", "x.", "x "] {
        assert!(
            theseus_host::fs::RelativePath::parse(good).is_ok(),
            "{good:?}"
        );
    }
}
