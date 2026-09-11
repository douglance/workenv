use std::collections::BTreeSet;

#[derive(Clone, Debug, Default)]
pub(super) struct ExtensionFlags {
    enabled: BTreeSet<&'static str>,
}

impl ExtensionFlags {
    pub fn contains(&self, extension: &'static str) -> bool {
        self.enabled.contains(extension)
    }

    pub fn enable(&mut self, extension: &'static str) {
        self.enabled.insert(extension);
    }
}
