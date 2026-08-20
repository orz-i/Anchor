use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildIdentity {
    pub package_version: String,
    pub git_sha: String,
    pub git_dirty: bool,
    pub build_workspace: String,
}

impl BuildIdentity {
    pub fn current() -> Self {
        Self {
            package_version: env!("CARGO_PKG_VERSION").into(),
            git_sha: env!("ANCHOR_BUILD_GIT_SHA").into(),
            git_dirty: env!("ANCHOR_BUILD_GIT_DIRTY") == "true",
            build_workspace: env!("ANCHOR_BUILD_WORKSPACE").into(),
        }
    }

    pub fn same_build(&self, other: &Self) -> bool {
        self.package_version == other.package_version
            && self.git_sha == other.git_sha
            && self.git_dirty == other.git_dirty
    }

    pub fn short_git_sha(&self) -> &str {
        self.git_sha.get(..8).unwrap_or(self.git_sha.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_identity_uses_embedded_git_metadata() {
        let identity = BuildIdentity::current();
        assert_eq!(identity.package_version, env!("CARGO_PKG_VERSION"));
        assert!(!identity.git_sha.trim().is_empty());
        assert!(!identity.build_workspace.trim().is_empty());
        assert_eq!(
            identity.short_git_sha().len(),
            identity.git_sha.len().min(8)
        );
    }

    #[test]
    fn same_build_ignores_checkout_path_but_not_source_identity() {
        let mut left = BuildIdentity::current();
        let mut right = left.clone();
        right.build_workspace = "another-workspace".into();
        assert!(left.same_build(&right));

        left.git_sha = "different".into();
        assert!(!left.same_build(&right));
    }
}
