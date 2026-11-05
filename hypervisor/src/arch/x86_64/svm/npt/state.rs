//! NPT leaf views.

use super::types::NptEntry;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NptView {
    /// Original guest page, `NX=0`: execution view.
    OriginalExec,
    /// Private shadow page, `NX=1`: read/write view.
    ShadowNx,
    /// Private shadow page, `NX=0`: execution view (patch included).
    ShadowExec,
}

impl NptView {
    pub const fn executes(self) -> bool {
        matches!(self, Self::OriginalExec | Self::ShadowExec)
    }
}

/// Build the leaf PTE for `view` over the page frame `pfn`.
///
/// `P/W/U` are always 1 (NPT ignores U/S but nested VMMs expect it, and there is
/// no read-disable); only `NX` changes.
pub fn view_pte(view: NptView, pfn: u64) -> u64 {
    let mut entry = NptEntry::new();

    entry.set_present(true);
    entry.set_write(true);
    entry.set_user(true);
    entry.set_pfn_4k(pfn);
    entry.set_nx(!view.executes());

    entry.as_raw()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PFN: u64 = 0x1234;

    #[test]
    fn real_exec_clears_nx() {
        let pte = NptEntry::from_raw(view_pte(NptView::OriginalExec, PFN));

        assert!(!pte.nx());
        assert_eq!(pte.pfn_4k(), PFN);
    }

    #[test]
    fn shadow_views_keep_the_requested_frame() {
        let no_exec = NptEntry::from_raw(view_pte(NptView::ShadowNx, 0x99));
        let exec = NptEntry::from_raw(view_pte(NptView::ShadowExec, 0x99));

        assert_eq!(no_exec.pfn_4k(), 0x99);
        assert!(no_exec.nx());
        assert_eq!(exec.pfn_4k(), 0x99);
        assert!(!exec.nx());
    }

    #[test]
    fn view_helpers() {
        assert!(NptView::OriginalExec.executes());
        assert!(NptView::ShadowExec.executes());
        assert!(!NptView::ShadowNx.executes());
    }
}
