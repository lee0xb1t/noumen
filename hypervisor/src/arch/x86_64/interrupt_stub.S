
.equ trap_frame_rax,   0
.equ trap_frame_rcx,   8
.equ trap_frame_rdx,   16
.equ trap_frame_rbx,   24
.equ trap_frame_rbp,   32
.equ trap_frame_rsi,   40
.equ trap_frame_rdi,   48
.equ trap_frame_r8,    56
.equ trap_frame_r9,    64
.equ trap_frame_r10,   72
.equ trap_frame_r11,   80
.equ trap_frame_r12,   88
.equ trap_frame_r13,   96
.equ trap_frame_r14,   104
.equ trap_frame_r15,   112
.equ trap_frame_vector,120
.equ trap_frame_error, 128
.equ trap_frame_rip,   136
.equ trap_frame_cs,    144
.equ trap_frame_rflags,152
.equ trap_frame_rsp,   160
.equ trap_frame_ss,    168


.section .text
.globl generic_interrupt_handler

.extern handle_host_interrupt_stub


generic_interrupt_handler:
    subq    $0x78, %rsp

    movq    %rax, trap_frame_rax(%rsp)
    movq    %rcx, trap_frame_rcx(%rsp)
    movq    %rdx, trap_frame_rdx(%rsp)
    movq    %rbx, trap_frame_rbx(%rsp)
    movq    %rbp, trap_frame_rbp(%rsp)
    movq    %rsi, trap_frame_rsi(%rsp)
    movq    %rdi, trap_frame_rdi(%rsp)
    movq    %r8,  trap_frame_r8(%rsp)
    movq    %r9,  trap_frame_r9(%rsp)
    movq    %r10, trap_frame_r10(%rsp)
    movq    %r11, trap_frame_r11(%rsp)
    movq    %r12, trap_frame_r12(%rsp)
    movq    %r13, trap_frame_r13(%rsp)
    movq    %r14, trap_frame_r14(%rsp)
    movq    %r15, trap_frame_r15(%rsp)

    # System V ABI
    movq    %rsp, %rdi
    call    handle_host_interrupt_stub

    movq    trap_frame_rax(%rsp), %rax
    movq    trap_frame_rcx(%rsp), %rcx
    movq    trap_frame_rdx(%rsp), %rdx
    movq    trap_frame_rbx(%rsp), %rbx
    movq    trap_frame_rbp(%rsp), %rbp
    movq    trap_frame_rsi(%rsp), %rsi
    movq    trap_frame_rdi(%rsp), %rdi
    movq    trap_frame_r8(%rsp),  %r8
    movq    trap_frame_r9(%rsp),  %r9
    movq    trap_frame_r10(%rsp), %r10
    movq    trap_frame_r11(%rsp), %r11
    movq    trap_frame_r12(%rsp), %r12
    movq    trap_frame_r13(%rsp), %r13
    movq    trap_frame_r14(%rsp), %r14
    movq    trap_frame_r15(%rsp), %r15

    addq    $0x78, %rsp

    # Skip vector and error code
    addq    $0x10, %rsp

    iretq


# Macro
.macro DEFINE_ISR vector, name
    .globl \name
    .type \name, @function
\name:
    pushq   $\vector
    jmp     generic_interrupt_handler
.endm

.macro DEFINE_ISR_NO_ERROR vector, name
    .globl \name
    .type \name, @function
\name:
    pushq   $0
    pushq   $\vector
    jmp     generic_interrupt_handler
.endm


# Define interrupts
DEFINE_ISR_NO_ERROR    0, interrupt_handler_0
DEFINE_ISR_NO_ERROR    1, interrupt_handler_1
DEFINE_ISR_NO_ERROR    2, interrupt_handler_2
DEFINE_ISR_NO_ERROR    3, interrupt_handler_3
DEFINE_ISR_NO_ERROR    4, interrupt_handler_4
DEFINE_ISR_NO_ERROR    5, interrupt_handler_5
DEFINE_ISR_NO_ERROR    6, interrupt_handler_6
DEFINE_ISR_NO_ERROR    7, interrupt_handler_7
DEFINE_ISR             8, interrupt_handler_8
DEFINE_ISR             10,interrupt_handler_10
DEFINE_ISR             11,interrupt_handler_11
DEFINE_ISR             12,interrupt_handler_12
DEFINE_ISR             13,interrupt_handler_13
DEFINE_ISR             14,interrupt_handler_14
DEFINE_ISR_NO_ERROR    16,interrupt_handler_16
DEFINE_ISR             17,interrupt_handler_17
DEFINE_ISR_NO_ERROR    18,interrupt_handler_18
DEFINE_ISR_NO_ERROR    19,interrupt_handler_19
DEFINE_ISR_NO_ERROR    20,interrupt_handler_20
DEFINE_ISR             30,interrupt_handler_30