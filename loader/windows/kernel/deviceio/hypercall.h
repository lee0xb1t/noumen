#pragma once

#include <ntddk.h>

#define NOUMEN_HYPERCALL_KEY          0x10F2CULL
#define NOUMEN_HYPERCALL_PING         3ULL
#define NOUMEN_HYPERCALL_STOP         4ULL
#define NOUMEN_HYPERCALL_HOOK_INSTALL 5ULL
#define NOUMEN_HYPERCALL_HOOK_REMOVE  6ULL
#define NOUMEN_HYPERCALL_HOOK_REMOVE_PAGE 7ULL
#define NOUMEN_HYPERCALL_HOOK_APPLY   20ULL
#define NOUMEN_HYPERCALL_USER_BREAKPOINT_COMMIT 21ULL
#define NOUMEN_HYPERCALL_HOOK_RETIRED 22ULL
#define NOUMEN_HYPERCALL_PONG_MAGIC   0x504F4E47UL

//
// SVM-only (NPT) debug methods; ids deliberately distinct from the VMX hook
// commands so the loader can keep both backends behind one UI.
//
#define NOUMEN_HYPERCALL_SVM_HOOK_INSTALL 0x30ULL
#define NOUMEN_HYPERCALL_SVM_HOOK_INSTALL_NPT 0x3AULL
#define NOUMEN_HYPERCALL_SVM_HOOK_REMOVE  0x31ULL
#define NOUMEN_HYPERCALL_SVM_TF_STEP      0x32ULL

#define NOUMEN_SVM_METHOD_BYTE_PATCH   0UL
#define NOUMEN_SVM_METHOD_COPY_EXEC    1UL
#define NOUMEN_SVM_METHOD_TF_STEP      2UL
#define NOUMEN_SVM_METHOD_NX_TRAP      3UL
#define NOUMEN_SVM_METHOD_DR_BREAKPOINT 4UL
#define NOUMEN_SVM_METHOD_NPT_HOOK      5UL

//
// NOUMEN_EPT_HOOK_REQUEST.Kind
//
// 断点在私有页副本里放隐藏的 0xCC；detour_internal 放一条绝对跳转到
// hypervisor 自己的上报 stub；detour_trampoline 放一条绝对跳转到驱动的
// NtCreateFile proxy，并让 proxy 通过 origin 页调用原函数；hide 让读写看到
// 全 0 私有页、执行运行真实页；user_breakpoint 同样是隐藏 0xCC，但命中后
// 停在 INT3 等调试器应答（只支持用户态地址）。detour/hide 只支持内核 VA
// （r3 断点/hide 通过目标进程 PID + attach + 锁页支持）。
//
#define NOUMEN_HOOK_KIND_BREAKPOINT 0UL
#define NOUMEN_HOOK_KIND_DETOUR_INTERNAL     1UL
#define NOUMEN_HOOK_KIND_HIDE       2UL
#define NOUMEN_HOOK_KIND_DETOUR_TRAMPOLINE 3UL
#define NOUMEN_HOOK_KIND_USER_BREAKPOINT 4UL

UINT64
noumen_vmcall(
    _In_ UINT64 Key,
    _In_ UINT64 Function,
    _In_ UINT64 Argument0,
    _In_ UINT64 Argument1
    );

UINT64
noumen_vmcall3(
    _In_ UINT64 Key,
    _In_ UINT64 Function,
    _In_ UINT64 Argument0,
    _In_ UINT64 Argument1,
    _In_ UINT64 Argument2
    );

UINT64
noumen_vmcall4(
    _In_ UINT64 Key,
    _In_ UINT64 Function,
    _In_ UINT64 Argument0,
    _In_ UINT64 Argument1,
    _In_ UINT64 Argument2,
    _In_ UINT64 Argument3
    );

UINT64
NoumenHypercallPing(
    _In_ UINT64 Key,
    _In_ UINT64 Nonce
    );

UINT64
NoumenHypercallHookInstall(
    _In_ UINT64 Key,
    _In_ UINT64 VirtualAddress,
    _In_ UINT64 Kind,
    _In_ UINT64 Handler,
    _In_ UINT64 Trampoline
    );

UINT64
NoumenHypercallHookRemove(
    _In_ UINT64 Key,
    _In_ UINT64 VirtualAddress
    );

UINT64
NoumenHypercallSvmHookInstall(
    _In_ UINT64 Key,
    _In_ UINT64 VirtualAddress,
    _In_ UINT64 Method,
    _In_ UINT64 Kind
    );

UINT64
NoumenHypercallSvmHookInstallNpt(
    _In_ UINT64 Key,
    _In_ UINT64 VirtualAddress,
    _In_ UINT64 Handler,
    _In_ UINT64 Trampoline,
    _In_ UINT64 Kind
    );

UINT64
NoumenHypercallSvmHookRemove(
    _In_ UINT64 Key,
    _In_ UINT64 VirtualAddress
    );

UINT64
NoumenHypercallSvmTfStep(
    _In_ UINT64 Key
    );

//
// 移除 guest 物理页 `PagePhysicalAddress` 上的所有 hook。loader 在虚拟地址
// 不再可用时使用（进程退出或关闭），因此不需要 CR3，也不需要 attach 进程。
// 这是**按页**操作，不是全局 remove-all。
//
UINT64
NoumenHypercallHookRemovePage(
    _In_ UINT64 Key,
    _In_ UINT64 PagePhysicalAddress
    );

//
// 停止**调用 CPU** 的虚拟化：Intel 上 hypervisor 执行 VMXOFF，AMD 上清
// EFER.SVME，然后从 hypercall 指令之后继续运行 guest。返回后该 CPU 不再
// 虚拟化，再次发 hypercall 会 #UD。
//
UINT64
NoumenHypercallStop(
    _In_ UINT64 Key
    );

//
// 让**调用 CPU**立即应用最新一代 hook 状态。安装用户断点时 loader 会按核轮询，
// 保证所有核先把 leaf 切到 clean 视图，之后才写 0xCC（避免可检测窗口）。
//
UINT64
NoumenHypercallHookApply(
    _In_ UINT64 Key
    );

//
// 返回 1 表示所有在线 CPU 都已应用（retire）最新一代 hook；驱动在移除
// DETOUR_TRAMPOLINE 后先逐核 HookApply，再轮询这里，最后才释放 origin 页。
//
UINT64
NoumenHypercallHookRetired(
    _In_ UINT64 Key
    );

//
// 所有核切到 clean 视图之后，把用户断点的 0xCC 真正写进真实页。
//
UINT64
NoumenHypercallUserBreakpointCommit(
    _In_ UINT64 Key,
    _In_ UINT64 VirtualAddress
    );
