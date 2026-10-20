//! Credentials and privilege
//!
//! Currently a placeholder for uid/gid, chmod, etc.
//! TODO: Implement vulnerability and patch

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Credentials {
    pub uid: u32,
    pub gid: u32,
}

impl Credentials {
    pub const ROOT: Credentials = Credentials { uid: 0, gid: 0 };

    pub fn is_root(&self) -> bool {
        self.uid == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_is_uid_zero() {
        assert!(Credentials::ROOT.is_root());
        let user = Credentials {
            uid: 1000,
            gid: 1000,
        };

        assert!(!user.is_root());
    }
}
