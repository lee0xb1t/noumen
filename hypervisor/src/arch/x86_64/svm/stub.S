# SVM entry/exit stub.
#
# Stack frame at `vmrun` (built by svm_vm_launch and reused by the loop):
#   0(%rsp)  VMCB physical address
#   8(%rsp)  HSAVE physical address
#  16(%rsp)  VMCB virtual address
#  24(%rsp)  *mut GuestContext
#
# Contract with Rust (`svm::state` / `svm::stub`):
#   - host state that VMRUN does not save (FS/GS/TR/LDTR/STAR/...) is snapshotted
#     with VMSAVE before entry and restored with VMLOAD after exit;
#   - #VMEXIT leaves guest RAX/RSP in the VMCB and the other GPRs in registers;
#   - GIF stays 0 while root runs; VMRUN sets it for the guest.

.equ GC_DR0,   144
.equ GC_DR1,   152
.equ GC_DR2,   160
.equ GC_DR3,   168
.equ GC_RAX,   0
.equ GC_RCX,   8
.equ GC_RDX,   16
.equ GC_RBX,   24
.equ GC_RBP,   40
.equ GC_RSI,   48
.equ GC_RDI,   56
.equ GC_R8,    64
.equ GC_R9,    72
.equ GC_R10,   80
.equ GC_R11,   88
.equ GC_R12,   96
.equ GC_R13,   104
.equ GC_R14,   112
.equ GC_R15,   120
.equ GC_CR2,   128
.equ GC_CR8,   136
# GuestContext 保留 XMM 区域（0xC0..0x1BF），但 stub 不保存 XMM；root 不得使用 SSE/XMM。
# Host DR values saved around VMRUN.
.equ GC_HOST_DR0, 448
.equ GC_HOST_DR1, 456
.equ GC_HOST_DR2, 464
.equ GC_HOST_DR3, 472
.equ GC_HOST_DR6, 480
.equ GC_HOST_DR7, 488

# VMCB state-save offsets (0x400 + relative offset).
.equ VMCB_SAVE_RIP, 0x578
.equ VMCB_SAVE_RSP, 0x5d8
.equ VMCB_SAVE_RAX, 0x5f8

.text
.globl svm_vm_launch
.type svm_vm_launch, @function
.extern handle_svm_exit

# void svm_vm_launch(u64 vmcb_va, u64 vmcb_pa, u64 host_rsp, u64 hsave_pa,
#                    *mut GuestContext ctx)
#
# The first guest instruction is svm_launch_return, executed with the caller's
# stack, so it returns out of this function and the caller keeps running as the
# guest (the same trick as VMX's `successful_launch`).
svm_vm_launch:
    movq    %rsp, VMCB_SAVE_RSP(%rdi)
    leaq    svm_launch_return(%rip), %rax
    movq    %rax, VMCB_SAVE_RIP(%rdi)

    movq    %rdx, %rsp
    pushq   %r8                     # 24(%rsp) ctx
    pushq   %rdi                    # 16(%rsp) vmcb_va
    pushq   %rcx                    # 8(%rsp)  hsave_pa
    pushq   %rsi                    # 0(%rsp)  vmcb_pa
    jmp     svm_resume

svm_launch_return:
    movl    $1, %eax
    ret

svm_resume:
    # VMRUN does NOT load the VMLOAD/VMSAVE subset (FS/GS/TR/LDTR/STAR/...),
    # so the guest copy must be loaded explicitly before every entry.
    movq    8(%rsp), %rax
    vmsave  %rax                    # snapshot the host copy
    movq    0(%rsp), %rax
    vmload  %rax                    # load the guest copy from the VMCB

    # DR0-DR3 are not part of the VMCB load/save subset; DR7 is loaded by VMRUN.
    # Save the host set, then load the effective guest set.
    # RCX is a guest GPR that VMRUN does not reload from the VMCB, so it must
    # survive this block; RAX is reloaded from VMCB_SAVE_RAX.
    movq    24(%rsp), %rax
    pushq   %rcx
    movq    %dr0, %rcx
    movq    %rcx, GC_HOST_DR0(%rax)
    movq    %dr1, %rcx
    movq    %rcx, GC_HOST_DR1(%rax)
    movq    %dr2, %rcx
    movq    %rcx, GC_HOST_DR2(%rax)
    movq    %dr3, %rcx
    movq    %rcx, GC_HOST_DR3(%rax)
    movq    %dr6, %rcx
    movq    %rcx, GC_HOST_DR6(%rax)
    movq    %dr7, %rcx
    movq    %rcx, GC_HOST_DR7(%rax)

    movq    GC_DR0(%rax), %rcx
    movq    %rcx, %dr0
    movq    GC_DR1(%rax), %rcx
    movq    %rcx, %dr1
    movq    GC_DR2(%rax), %rcx
    movq    %rcx, %dr2
    movq    GC_DR3(%rax), %rcx
    movq    %rcx, %dr3
    popq    %rcx

    movq    0(%rsp), %rax
    vmrun   %rax

svm_exit:
    # #VMEXIT: hardware already wrote the guest state back to the VMCB.
    movq    0(%rsp), %rax
    vmsave  %rax                    # guest VMLOAD/VMSAVE state
    movq    8(%rsp), %rax
    vmload  %rax                    # host VMLOAD/VMSAVE state (FS base included)

    # GIF stays 0 in root: an NMI taken here would run on the OS's IDT with host
    # state. VMRUN sets GIF=1 for the guest and #VMEXIT clears it again.

    # Save guest GPRs. RAX comes from the VMCB; RSP stays there.
    movq    24(%rsp), %rax
    movq    %rbx, GC_RBX(%rax)
    movq    %rcx, GC_RCX(%rax)
    movq    %rdx, GC_RDX(%rax)
    movq    %rbp, GC_RBP(%rax)
    movq    %rsi, GC_RSI(%rax)
    movq    %rdi, GC_RDI(%rax)
    movq    %r8,  GC_R8(%rax)
    movq    %r9,  GC_R9(%rax)
    movq    %r10, GC_R10(%rax)
    movq    %r11, GC_R11(%rax)
    movq    %r12, GC_R12(%rax)
    movq    %r13, GC_R13(%rax)
    movq    %r14, GC_R14(%rax)
    movq    %r15, GC_R15(%rax)

    movq    16(%rsp), %rdx
    movq    VMCB_SAVE_RAX(%rdx), %rcx
    movq    %rcx, GC_RAX(%rax)

    # Save the guest DR0-3, then restore the host set before entering Rust.
    movq    %dr0, %rcx
    movq    %rcx, GC_DR0(%rax)
    movq    %dr1, %rcx
    movq    %rcx, GC_DR1(%rax)
    movq    %dr2, %rcx
    movq    %rcx, GC_DR2(%rax)
    movq    %dr3, %rcx
    movq    %rcx, GC_DR3(%rax)

    movq    GC_HOST_DR0(%rax), %rcx
    movq    %rcx, %dr0
    movq    GC_HOST_DR1(%rax), %rcx
    movq    %rcx, %dr1
    movq    GC_HOST_DR2(%rax), %rcx
    movq    %rcx, %dr2
    movq    GC_HOST_DR3(%rax), %rcx
    movq    %rcx, %dr3
    movq    GC_HOST_DR6(%rax), %rcx
    movq    %rcx, %dr6
    movq    GC_HOST_DR7(%rax), %rcx
    movq    %rcx, %dr7

    movq    %rax, %rdi
    call    handle_svm_exit
    testb   %al, %al
    jnz     svm_halt

    # Write guest RAX back to the VMCB, then restore the other GPRs.
    movq    24(%rsp), %rax
    movq    16(%rsp), %rdx
    movq    GC_RAX(%rax), %rcx
    movq    %rcx, VMCB_SAVE_RAX(%rdx)

    movq    GC_RBX(%rax), %rbx
    movq    GC_RBP(%rax), %rbp
    movq    GC_RSI(%rax), %rsi
    movq    GC_RDI(%rax), %rdi
    movq    GC_R8(%rax),  %r8
    movq    GC_R9(%rax),  %r9
    movq    GC_R10(%rax), %r10
    movq    GC_R11(%rax), %r11
    movq    GC_R12(%rax), %r12
    movq    GC_R13(%rax), %r13
    movq    GC_R14(%rax), %r14
    movq    GC_R15(%rax), %r15
    movq    GC_RCX(%rax), %rcx
    movq    GC_RDX(%rax), %rdx

    jmp     svm_resume

svm_halt:
    cli
1:  hlt
    jmp     1b

# void svm_vmload(u64 vmcb_pa)
# Load the guest VMLOAD/VMSAVE state (FS/GS/TR/LDTR/STAR/...) before leaving
# virtualization. Requires EFER.SVME=1.
.globl svm_vmload
.type svm_vmload, @function
svm_vmload:
    movq    %rdi, %rax
    vmload  %rax
    ret
.size svm_vmload, .-svm_vmload

# struct DevirtFrame {
#   u64 cr0, cr3, cr4, efer, rip, rsp, rflags, cs, ss;
# }
#
# void svm_leave_to_guest(const struct DevirtFrame *frame) __attribute__((noreturn));
#
# Loads guest control registers, clears EFER.SVME (the frame already has it
# cleared), then iretqs to the guest. GIF is opened just before clearing SVME:
# STGI is an SVM instruction and would #UD afterwards.
.globl svm_leave_to_guest
.type svm_leave_to_guest, @function
svm_leave_to_guest:
    movq    8(%rdi), %rax
    movq    %rax, %cr3
    movq    0(%rdi), %rax
    movq    %rax, %cr0
    movq    16(%rdi), %rax
    movq    %rax, %cr4

    stgi
    movq    24(%rdi), %rax
    movq    %rax, %rdx
    shrq    $32, %rdx
    movl    $0xc0000080, %ecx
    wrmsr

    pushq   64(%rdi)                # SS
    pushq   40(%rdi)                # RSP
    pushq   48(%rdi)                # RFLAGS
    pushq   56(%rdi)                # CS
    pushq   32(%rdi)                # RIP

    # The VMRUN exit path saved the guest GPRs before entering Rust. Devirt
    # bypasses that restore loop, so reload them here (RSI last: it holds the
    # GuestContext pointer until the final load).
    movq    GC_RBX(%rsi), %rbx
    movq    GC_RBP(%rsi), %rbp
    movq    GC_RDI(%rsi), %rdi
    movq    GC_R8(%rsi),  %r8
    movq    GC_R9(%rsi),  %r9
    movq    GC_R10(%rsi), %r10
    movq    GC_R11(%rsi), %r11
    movq    GC_R12(%rsi), %r12
    movq    GC_R13(%rsi), %r13
    movq    GC_R14(%rsi), %r14
    movq    GC_R15(%rsi), %r15
    movq    GC_RAX(%rsi), %rax
    movq    GC_RCX(%rsi), %rcx
    movq    GC_RDX(%rsi), %rdx
    movq    GC_RSI(%rsi), %rsi

    iretq
.size svm_leave_to_guest, .-svm_leave_to_guest

.size svm_vm_launch, .-svm_vm_launch
