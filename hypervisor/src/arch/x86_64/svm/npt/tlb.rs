//! TLB invalidation for NPT changes.

use super::vmcb::{Vmcb, access::TlbControl};

/// 下一次进入 guest 时让本 guest 的 ASID 失效。
pub fn flush_asid(vmcb: &mut Vmcb) {
    TlbControl::request(TlbControl::FLUSH_ASID).write(vmcb);
}
