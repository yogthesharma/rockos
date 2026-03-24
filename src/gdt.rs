//! Global Descriptor Table + Task State Segment (IST for `#DF`, RSP0 for IRQs from ring 3).

use core::mem::size_of_val;
use core::ptr::addr_of;
use spin::Lazy;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;
use x86_64::VirtAddr;

/// IST slot used for `#DF` so a corrupt kernel stack does not triple-fault.
pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;

#[repr(C, align(4096))]
struct DoubleFaultStack([u8; 4096 * 5]);

static DOUBLE_FAULT_STACK: DoubleFaultStack = DoubleFaultStack([0; 4096 * 5]);

/// CPL-0 stack when an interrupt arrives while the CPU is in **ring 3** (`TSS.RSP0`).
#[repr(C, align(4096))]
struct Privilege0Stack([u8; 4096 * 4]);

static PRIVILEGE0_STACK: Privilege0Stack = Privilege0Stack([0; 4096 * 4]);

pub static TSS: Lazy<TaskStateSegment> = Lazy::new(|| {
    let mut tss = TaskStateSegment::new();
    let df_bottom = VirtAddr::new(addr_of!(DOUBLE_FAULT_STACK.0) as u64);
    tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] =
        df_bottom + size_of_val(&DOUBLE_FAULT_STACK.0);

    let p0_bottom = VirtAddr::new(addr_of!(PRIVILEGE0_STACK.0) as u64);
    tss.privilege_stack_table[0] = p0_bottom + size_of_val(&PRIVILEGE0_STACK.0);
    tss
});

pub struct Selectors {
    pub kernel_code_segment: SegmentSelector,
    pub kernel_data_segment: SegmentSelector,
    pub user_data_segment: SegmentSelector,
    pub user_code_segment: SegmentSelector,
    pub tss_selector: SegmentSelector,
}

pub static GDT: Lazy<(GlobalDescriptorTable, Selectors)> = Lazy::new(|| {
    let mut gdt = GlobalDescriptorTable::new();
    let kernel_code_segment = gdt.add_entry(Descriptor::kernel_code_segment());
    let kernel_data_segment = gdt.add_entry(Descriptor::kernel_data_segment());
    let user_data_segment = gdt.add_entry(Descriptor::user_data_segment());
    let user_code_segment = gdt.add_entry(Descriptor::user_code_segment());
    let tss_selector = gdt.add_entry(Descriptor::tss_segment(&*TSS));
    (
        gdt,
        Selectors {
            kernel_code_segment,
            kernel_data_segment,
            user_data_segment,
            user_code_segment,
            tss_selector,
        },
    )
});

pub fn init() {
    use x86_64::instructions::segmentation::{Segment, CS, DS, ES, SS};
    use x86_64::instructions::tables::load_tss;

    GDT.0.load();
    unsafe {
        CS::set_reg(GDT.1.kernel_code_segment);
        SS::set_reg(GDT.1.kernel_data_segment);
        DS::set_reg(GDT.1.kernel_data_segment);
        ES::set_reg(GDT.1.kernel_data_segment);
        load_tss(GDT.1.tss_selector);
    }
}
