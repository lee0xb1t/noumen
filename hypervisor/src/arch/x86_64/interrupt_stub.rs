use crate::arch::x86_64::interrupt_handler::handle_host_interrupt;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TrapFrame {
    pub rax: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rbx: u64,
    pub rbp: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub vector: u64,
    pub error: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

unsafe extern "C" {
    pub unsafe fn interrupt_handler_0();
    pub unsafe fn interrupt_handler_1();
    pub unsafe fn interrupt_handler_2();
    pub unsafe fn interrupt_handler_3();
    pub unsafe fn interrupt_handler_4();
    pub unsafe fn interrupt_handler_5();
    pub unsafe fn interrupt_handler_6();
    pub unsafe fn interrupt_handler_7();
    pub unsafe fn interrupt_handler_8();
    pub unsafe fn interrupt_handler_10();
    pub unsafe fn interrupt_handler_11();
    pub unsafe fn interrupt_handler_12();
    pub unsafe fn interrupt_handler_13();
    pub unsafe fn interrupt_handler_14();
    pub unsafe fn interrupt_handler_16();
    pub unsafe fn interrupt_handler_17();
    pub unsafe fn interrupt_handler_18();
    pub unsafe fn interrupt_handler_19();
    pub unsafe fn interrupt_handler_20();
    pub unsafe fn interrupt_handler_30();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn handle_host_interrupt_stub(frame: *mut TrapFrame) {
    let trap_frame = unsafe { &mut *frame };
    handle_host_interrupt(trap_frame);
}
