# noumen 初始化专用栈。
#
# loader 线程的内核栈很小（Windows x64 通常十几 KiB），而 `_init` 的调用链
# （VMCS/host/guest 状态 + EPT 建表 + `_klog_fmt` 的 0x130 帧 + fmt 临时量）
# 会把它用到接近守卫页。0x7F (DOUBLE_FAULT) 就是这么来的：RSP 踩进未映射的
# 栈底页，随后 DPC 中断压 trap frame 时二次异常。
#
# 这里给初始化准备一块 64 KiB 静态栈，`run_on_init_stack` 保存调用者的 rsp、
# 切栈、调用 `init_body`，返回前恢复。虚拟化之后 guest 继续用这块栈，直到
# `init_body` 返回、`_init` 把 rsp 还给 loader。

.text
.globl run_on_init_stack
.type run_on_init_stack, @function
.extern init_body

run_on_init_stack:
    pushq   %rbp
    movq    %rsp, %rbp                  # 保存 loader 的栈指针
    leaq    init_stack_top(%rip), %rsp
    andq    $-16, %rsp
    call    init_body                   # edi 仍是 cpu_id；返回后 rsp 不变
    movq    %rbp, %rsp
    popq    %rbp
    ret

.bss
.align 16
init_stack:
    .skip 65536
.globl init_stack_top
init_stack_top:
