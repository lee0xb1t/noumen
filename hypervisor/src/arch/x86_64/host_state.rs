use core::mem::size_of;

use x86_64::structures::{gdt, idt, tss};

use crate::arch::x86_64::{addr::VirtAddr, interrupt_stub, memory, segment};
use crate::error::HvResult;

const STACK_PAGES: usize = 4;
const STACK_HEADROOM: usize = 16;

pub struct HostState {
    idt: memory::Page<idt::InterruptDescriptorTable>,
    gdt: memory::Page<gdt::GlobalDescriptorTable<{ segment::MAX_GDT_ENTRY }>>,
    tss: memory::Page<tss::TaskStateSegment>,
    stack: memory::Page<u8>,
    pub queued_nmis: u64,
}

impl HostState {
    pub const fn new() -> Self {
        Self {
            idt: memory::Page::empty(),
            gdt: memory::Page::empty(),
            tss: memory::Page::empty(),
            stack: memory::Page::empty(),
            queued_nmis: 0,
        }
    }

    pub fn init(&mut self) -> HvResult<()> {
        self.idt = memory::alloc()?;
        self.gdt = memory::alloc()?;
        self.tss = memory::alloc()?;
        self.stack = memory::alloc_pages(STACK_PAGES)?;

        Ok(())
    }

    fn idt_page(&self) -> memory::Page<idt::InterruptDescriptorTable> {
        self.idt
    }

    fn gdt_page(&self) -> memory::Page<gdt::GlobalDescriptorTable<{ segment::MAX_GDT_ENTRY }>> {
        self.gdt
    }

    fn tss_page(&self) -> memory::Page<tss::TaskStateSegment> {
        self.tss
    }

    fn stack_page(&self) -> memory::Page<u8> {
        self.stack
    }

    pub fn idt_base(&self) -> VirtAddr {
        self.idt_page().virt()
    }

    pub fn gdt_base(&self) -> VirtAddr {
        self.gdt_page().virt()
    }

    pub fn tss_base(&self) -> VirtAddr {
        self.tss_page().virt()
    }

    pub fn rsp(&self) -> VirtAddr {
        let stack = self.stack_page();

        VirtAddr::new(stack.virt().as_u64() + (stack.size() - STACK_HEADROOM) as u64)
    }

    pub fn prepare_gdt(&mut self) {
        let tss_page = self.tss_page();
        let gdt = unsafe { &mut *self.gdt_page().as_mut_ptr() };
        let tss = unsafe { &mut *tss_page.as_mut_ptr() };

        *gdt = gdt::GlobalDescriptorTable::<{ segment::MAX_GDT_ENTRY }>::empty();
        tss.iomap_base = size_of::<tss::TaskStateSegment>() as u16;

        let cs = gdt.append(gdt::Descriptor::kernel_code_segment());
        let ss = gdt.append(gdt::Descriptor::kernel_data_segment());
        let tr = gdt.append(unsafe { gdt::Descriptor::tss_segment_unchecked(tss_page.as_ptr()) });

        debug_assert_eq!(cs.index(), segment::HOST_CS_SELECTOR.index());
        debug_assert_eq!(ss.index(), segment::HOST_SS_SELECTOR.index());
        debug_assert_eq!(tr.index(), segment::HOST_TR_SELECTOR.index());
    }

    unsafe fn install_handler<F>(entry: &mut idt::Entry<F>, handler: *const ()) {
        *entry = idt::Entry::missing();
        let options = unsafe { entry.set_handler_addr(x86_64::VirtAddr::new(handler as u64)) };
        unsafe { options.set_code_selector(segment::HOST_CS_SELECTOR) };
    }

    pub fn prepare_idt(&mut self) {
        let idt = unsafe { &mut *self.idt_page().as_mut_ptr() };

        unsafe {
            Self::install_handler(
                &mut idt.divide_error,
                interrupt_stub::interrupt_handler_0 as *const (),
            );
            Self::install_handler(
                &mut idt.debug,
                interrupt_stub::interrupt_handler_1 as *const (),
            );
            Self::install_handler(
                &mut idt.non_maskable_interrupt,
                interrupt_stub::interrupt_handler_2 as *const (),
            );
            Self::install_handler(
                &mut idt.breakpoint,
                interrupt_stub::interrupt_handler_3 as *const (),
            );
            Self::install_handler(
                &mut idt.overflow,
                interrupt_stub::interrupt_handler_4 as *const (),
            );
            Self::install_handler(
                &mut idt.bound_range_exceeded,
                interrupt_stub::interrupt_handler_5 as *const (),
            );
            Self::install_handler(
                &mut idt.invalid_opcode,
                interrupt_stub::interrupt_handler_6 as *const (),
            );
            Self::install_handler(
                &mut idt.device_not_available,
                interrupt_stub::interrupt_handler_7 as *const (),
            );
            Self::install_handler(
                &mut idt.double_fault,
                interrupt_stub::interrupt_handler_8 as *const (),
            );
            Self::install_handler(
                &mut idt.invalid_tss,
                interrupt_stub::interrupt_handler_10 as *const (),
            );
            Self::install_handler(
                &mut idt.segment_not_present,
                interrupt_stub::interrupt_handler_11 as *const (),
            );
            Self::install_handler(
                &mut idt.stack_segment_fault,
                interrupt_stub::interrupt_handler_12 as *const (),
            );
            Self::install_handler(
                &mut idt.general_protection_fault,
                interrupt_stub::interrupt_handler_13 as *const (),
            );
            Self::install_handler(
                &mut idt.page_fault,
                interrupt_stub::interrupt_handler_14 as *const (),
            );
            Self::install_handler(
                &mut idt.x87_floating_point,
                interrupt_stub::interrupt_handler_16 as *const (),
            );
            Self::install_handler(
                &mut idt.alignment_check,
                interrupt_stub::interrupt_handler_17 as *const (),
            );
            Self::install_handler(
                &mut idt.machine_check,
                interrupt_stub::interrupt_handler_18 as *const (),
            );
            Self::install_handler(
                &mut idt.simd_floating_point,
                interrupt_stub::interrupt_handler_19 as *const (),
            );
            Self::install_handler(
                &mut idt.virtualization,
                interrupt_stub::interrupt_handler_20 as *const (),
            );
            Self::install_handler(
                &mut idt.security_exception,
                interrupt_stub::interrupt_handler_30 as *const (),
            );
        }
    }
}
