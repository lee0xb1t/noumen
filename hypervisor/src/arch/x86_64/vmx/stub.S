# GuestContext 的字段偏移(与 Rust 侧的 #[repr(C)] 布局一致). 
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
.equ GC_DR0,   144
.equ GC_DR1,   152
.equ GC_DR2,   160
.equ GC_DR3,   168
.equ GC_DR6,   176
.equ GC_XMM0,  192      # 16 bytes align
.equ GC_XMM1,  208
.equ GC_XMM2,  224
.equ GC_XMM3,  240
.equ GC_XMM4,  256
.equ GC_XMM5,  272
.equ GC_XMM6,  288
.equ GC_XMM7,  304
.equ GC_XMM8,  320
.equ GC_XMM9,  336
.equ GC_XMM10, 352
.equ GC_XMM11, 368
.equ GC_XMM12, 384
.equ GC_XMM13, 400
.equ GC_XMM14, 416
.equ GC_XMM15, 432

.text
.globl vm_exit
.type vm_exit, @function
.globl vm_launch
.type vm_launch, @function

.extern handle_vm_exit_stub
.extern fatal_vmresume_error

vm_exit:
    subq    $0x1C0, %rsp

    movq    %rax, GC_RAX(%rsp)
    movq    %rdx, GC_RDX(%rsp)

    movq    %rcx, GC_RCX(%rsp)
    movq    %rbx, GC_RBX(%rsp)
    movq    %rbp, GC_RBP(%rsp)
    movq    %rsi, GC_RSI(%rsp)
    movq    %rdi, GC_RDI(%rsp)
    movq    %r8,  GC_R8(%rsp)
    movq    %r9,  GC_R9(%rsp)
    movq    %r10, GC_R10(%rsp)
    movq    %r11, GC_R11(%rsp)
    movq    %r12, GC_R12(%rsp)
    movq    %r13, GC_R13(%rsp)
    movq    %r14, GC_R14(%rsp)
    movq    %r15, GC_R15(%rsp)

    movq    %cr2, %rax
    movq    %rax, GC_CR2(%rsp)
    movq    %cr8, %rax
    movq    %rax, GC_CR8(%rsp)

    movq    %dr0, %rax
    movq    %rax, GC_DR0(%rsp)
    movq    %dr1, %rax
    movq    %rax, GC_DR1(%rsp)
    movq    %dr2, %rax
    movq    %rax, GC_DR2(%rsp)
    movq    %dr3, %rax
    movq    %rax, GC_DR3(%rsp)
    movq    %dr6, %rax
    movq    %rax, GC_DR6(%rsp)

    movaps  %xmm0, GC_XMM0(%rsp)
    movaps  %xmm1, GC_XMM1(%rsp)
    movaps  %xmm2, GC_XMM2(%rsp)
    movaps  %xmm3, GC_XMM3(%rsp)
    movaps  %xmm4, GC_XMM4(%rsp)
    movaps  %xmm5, GC_XMM5(%rsp)
    movaps  %xmm6, GC_XMM6(%rsp)
    movaps  %xmm7, GC_XMM7(%rsp)
    movaps  %xmm8, GC_XMM8(%rsp)
    movaps  %xmm9, GC_XMM9(%rsp)
    movaps  %xmm10, GC_XMM10(%rsp)
    movaps  %xmm11, GC_XMM11(%rsp)
    movaps  %xmm12, GC_XMM12(%rsp)
    movaps  %xmm13, GC_XMM13(%rsp)
    movaps  %xmm14, GC_XMM14(%rsp)
    movaps  %xmm15, GC_XMM15(%rsp)

    movq    %rsp, %rdi
    call    handle_vm_exit_stub

    movq    %rax, %r15

    movaps  GC_XMM0(%rsp), %xmm0
    movaps  GC_XMM1(%rsp), %xmm1
    movaps  GC_XMM2(%rsp), %xmm2
    movaps  GC_XMM3(%rsp), %xmm3
    movaps  GC_XMM4(%rsp), %xmm4
    movaps  GC_XMM5(%rsp), %xmm5
    movaps  GC_XMM6(%rsp), %xmm6
    movaps  GC_XMM7(%rsp), %xmm7
    movaps  GC_XMM8(%rsp), %xmm8
    movaps  GC_XMM9(%rsp), %xmm9
    movaps  GC_XMM10(%rsp), %xmm10
    movaps  GC_XMM11(%rsp), %xmm11
    movaps  GC_XMM12(%rsp), %xmm12
    movaps  GC_XMM13(%rsp), %xmm13
    movaps  GC_XMM14(%rsp), %xmm14
    movaps  GC_XMM15(%rsp), %xmm15

    #movq    %rax, %r15

    movq    GC_DR0(%rsp), %rax
    movq    %rax, %dr0
    movq    GC_DR1(%rsp), %rax
    movq    %rax, %dr1
    movq    GC_DR2(%rsp), %rax
    movq    %rax, %dr2
    movq    GC_DR3(%rsp), %rax
    movq    %rax, %dr3
    movq    GC_DR6(%rsp), %rax
    movq    %rax, %dr6

    movq    GC_CR2(%rsp), %rax
    movq    %rax, %cr2
    movq    GC_CR8(%rsp), %rax
    movq    %rax, %cr8

    testb   %r15b, %r15b
    movq    GC_R15(%rsp), %r15
    jnz     vm_stop

    # VM-entry 采样：尽可能贴近 VMRESUME。此时 guest 的 rax/rdx 还没恢复，
    # 可以安全借用；结果写进 Vcpu 的第一个字段（%fs = Vcpu）。
    lfence
    rdtsc
    lfence
    movl    %eax, %fs:0
    movl    %edx, %fs:4

    movq    GC_RAX(%rsp), %rax
    movq    GC_RCX(%rsp), %rcx
    movq    GC_RDX(%rsp), %rdx
    movq    GC_RBX(%rsp), %rbx
    movq    GC_RBP(%rsp), %rbp
    movq    GC_RSI(%rsp), %rsi
    movq    GC_RDI(%rsp), %rdi
    movq    GC_R8(%rsp),  %r8
    movq    GC_R9(%rsp),  %r9
    movq    GC_R10(%rsp), %r10
    movq    GC_R11(%rsp), %r11
    movq    GC_R12(%rsp), %r12
    movq    GC_R13(%rsp), %r13
    movq    GC_R14(%rsp), %r14

    addq    $0x1C0, %rsp
    vmresume

    # VMRESUME 失败时控制流落到这里。不处理就会继续往下掉进 vm_stop，
    # 用一份没在跑的 guest 状态 iretq，直接三重故障。报告指令错误后停机.
    movl    $0x4400, %eax               # VM_INSTRUCTION_ERROR
    vmread  %rax, %rdi
    call    fatal_vmresume_error

# vm_stop: 搭 iretq 帧 (复用 GuestContext 的前 0x28 字节), VMXOFF, 还 CR0/CR4, 然后 iretq.
vm_stop:
    # 下面要弄脏 rax/rdx/rbp, 先存回去.
    pushq   %rax
    pushq   %rdx
    pushq   %rbp
    leaq    0x38(%rsp), %rbp            # rbp = ctx + 0x20

    movq    $0x0804, %rdx               # GUEST_SS_SELECTOR
    vmread  %rdx, %rax
    movq    %rax, 0x00(%rbp)

    movq    $0x681C, %rdx               # GUEST_RSP
    vmread  %rdx, %rax
    movq    %rax, -0x08(%rbp)

    movq    $0x6820, %rdx               # GUEST_RFLAGS
    vmread  %rdx, %rax
    movq    %rax, -0x10(%rbp)

    movq    $0x0802, %rdx               # GUEST_CS_SELECTOR
    vmread  %rdx, %rax
    movq    %rax, -0x18(%rbp)

    movq    $0x681E, %rdx               # GUEST_RIP
    vmread  %rdx, %rax
    movq    %rax, -0x20(%rbp)

    movq    $0x6004, %rax               # CR0_READ_SHADOW
    vmread  %rax, %rax
    movq    $0x6006, %rdx               # CR4_READ_SHADOW
    vmread  %rdx, %rdx

    vmxoff

    movq    %rax, %cr0
    movq    %rdx, %cr4

    popq    %rbp
    popq    %rdx
    popq    %rax

    iretq

vm_launch:
    pushq %rdx
    movq $0x681C, %rax
    vmwrite %rsp, %rax
    jc vmwrite_failed
    jz vmwrite_failed

    movq $0x681E, %rax
    leaq successful_launch(%rip), %rdx
    vmwrite %rdx, %rax
    jc vmwrite_failed
    jz vmwrite_failed

    vmlaunch

    popq %rdx
    xorq %rax, %rax
    ret

successful_launch:
    popq %rdx

    mov $1, %al
    ret

vmwrite_failed:
    popq %rdx
    xorq %rax, %rax
    ret
