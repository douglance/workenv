use std::collections::BTreeSet;

#[derive(Clone, Debug, Default)]
pub(super) struct ExtensionFlags {
    enabled: BTreeSet<&'static str>,
}

impl ExtensionFlags {
    pub fn enable(&mut self, extension: &'static str) {
        self.enabled.insert(extension);
    }

    /// Withdraw a flag for an extension this repository turned out not to ship.
    pub fn disable(&mut self, extension: &str) {
        self.enabled.remove(extension);
    }

    /// The enabled names, so the renderer does not keep a second list of them.
    pub fn names(&self) -> impl Iterator<Item = &&'static str> {
        self.enabled.iter()
    }
}
