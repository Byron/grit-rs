//! Various functionality related to git references
#![forbid(unsafe_code)]
#![deny(rust_2018_idioms)]
#![allow(missing_docs)]

pub mod reference {
    pub mod name {
        use quick_error::quick_error;

        quick_error! {
            #[derive(Debug)]
            pub enum Error {
                Tag(err: crate::tag::name::Error) {
                    display("A reference must be a valid tag name as well")
                    from()
                    source(err)
                }
                SomeLowercase {
                    display("Standalone references must be all uppercased, like 'HEAD'")
                }
                StartsWithSlash {
                    display("A reference name must not start with a slash '/'")
                }
                RepeatedSlash {
                    display("Multiple slashes in a row are not allowed as they may change the reference's meaning")
                }
                SingleDot {
                    display("Names must not be a single '.', but may contain it.")
                }

            }
        }
    }

    use bstr::BStr;
    pub fn name(path: &BStr) -> Result<&BStr, name::Error> {
        crate::tagname(path)?;
        if path[0] == b'/' {
            return Err(name::Error::StartsWithSlash);
        }
        let mut previous = 0;
        let mut one_before_previous = 0;
        let mut saw_slash = false;
        for byte in path.iter() {
            match *byte {
                b'/' if previous == b'.' && one_before_previous == b'/' => return Err(name::Error::SingleDot),
                b'/' if previous == b'/' => return Err(name::Error::RepeatedSlash),
                _ => {}
            }

            if *byte == b'/' {
                saw_slash = true;
            }
            one_before_previous = previous;
            previous = *byte;
        }

        if !saw_slash {
            if path.iter().any(|c| !c.is_ascii_uppercase()) {
                return Err(name::Error::SomeLowercase);
            }
        }
        Ok(path)
    }
}
pub use reference::name as refname;

pub mod tag {
    use bstr::BStr;
    ///
    pub mod name {
        use bstr::BString;
        use quick_error::quick_error;

        quick_error! {
            /// The error returned by [`name()`]
            #[derive(Debug)]
            #[allow(missing_docs)]
            pub enum Error {
                InvalidByte(name: BString) {
                    display("A ref must not contain invalid bytes or ascii control characters: '{}'", name)
                }
                DoubleDot {
                    display("A ref must not contain '..' as it may be mistaken for a range")
                }
                LockFileSuffix {
                    display("A ref must not end with '.lock'")
                }
                ReflogPortion {
                    display("A ref must not contain '@{{' which is a part of a ref-log")
                }
                Asterisk {
                    display("A ref must not contain '*' character")
                }
                StartsWithDot {
                    display("A ref must not start with a '.'")
                }
                EndsWithSlash {
                    display("A ref must not end with a '/'")
                }
                Empty {
                    display("A ref must not be empty")
                }
            }
        }
    }

    /// Assure the given `bytes` resemble a valid git ref name, which are returned unchanged on success.
    pub fn name(bytes: &BStr) -> Result<&BStr, name::Error> {
        if bytes.is_empty() {
            return Err(name::Error::Empty);
        }
        if *bytes.last().expect("non-empty") == b'/' {
            return Err(name::Error::EndsWithSlash);
        }

        let mut previous = 0;
        for byte in bytes.iter() {
            match byte {
                b'\\' | b'^' | b':' | b'[' | b'?' | b' ' | b'~' | b'\0'..=b'\x1F' | b'\x7F' => {
                    return Err(name::Error::InvalidByte(bytes.into()))
                }
                b'*' => return Err(name::Error::Asterisk),
                b'.' if previous == b'.' => return Err(name::Error::DoubleDot),
                b'{' if previous == b'@' => return Err(name::Error::ReflogPortion),
                _ => {}
            }
            previous = *byte;
        }
        if bytes[0] == b'.' {
            return Err(name::Error::StartsWithDot);
        }
        if bytes.ends_with(b".lock") {
            return Err(name::Error::LockFileSuffix);
        }
        Ok(bytes)
    }
}
pub use tag::name as tagname;