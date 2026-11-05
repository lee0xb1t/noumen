#include "hypercall.h"

UINT64
NoumenHypercallPing(
    _In_ UINT64 Key,
    _In_ UINT64 Nonce
    )
{
    return noumen_vmcall(Key, NOUMEN_HYPERCALL_PING, Nonce, 0);
}

UINT64
NoumenHypercallStop(
    _In_ UINT64 Key
    )
{
    //
    // No return-value semantics: on success this CPU is already outside VMX
    // operation and resumes right after the VMCALL (RAX still holds the key).
    //
    return noumen_vmcall(Key, NOUMEN_HYPERCALL_STOP, 0, 0);
}

UINT64
NoumenHypercallHookInstall(
    _In_ UINT64 Key,
    _In_ UINT64 VirtualAddress,
    _In_ UINT64 Kind,
    _In_ UINT64 Handler,
    _In_ UINT64 Trampoline
    )
{
    //
    // 返回值是 hypervisor 的 EPT hook 状态：0 成功，否则是 HookError 编号。
    // 它占用 RAX，而不是 key。Handler 通过 R9、origin trampoline 通过 R10 传入；
    // 四个参数走 `noumen_vmcall4`。旧调用方把 Trampoline 填 0。
    //
    return noumen_vmcall4(
        Key,
        NOUMEN_HYPERCALL_HOOK_INSTALL,
        VirtualAddress,
        Kind,
        Handler,
        Trampoline);
}

UINT64
NoumenHypercallHookRemove(
    _In_ UINT64 Key,
    _In_ UINT64 VirtualAddress
    )
{
    return noumen_vmcall(Key, NOUMEN_HYPERCALL_HOOK_REMOVE, VirtualAddress, 0);
}

UINT64
NoumenHypercallSvmHookInstall(
    _In_ UINT64 Key,
    _In_ UINT64 VirtualAddress,
    _In_ UINT64 Method,
    _In_ UINT64 Kind
    )
{
    // rdx=va, r8=method, r9=kind.
    return noumen_vmcall3(
        Key,
        NOUMEN_HYPERCALL_SVM_HOOK_INSTALL,
        VirtualAddress,
        Method,
        Kind);
}

UINT64
NoumenHypercallSvmHookRemove(
    _In_ UINT64 Key,
    _In_ UINT64 VirtualAddress
    )
{
    return noumen_vmcall(Key, NOUMEN_HYPERCALL_SVM_HOOK_REMOVE, VirtualAddress, 0);
}

UINT64
NoumenHypercallSvmHookInstallNpt(
    _In_ UINT64 Key,
    _In_ UINT64 VirtualAddress,
    _In_ UINT64 Handler,
    _In_ UINT64 Trampoline,
    _In_ UINT64 Kind
    )
{
    return noumen_vmcall4(
        Key,
        NOUMEN_HYPERCALL_SVM_HOOK_INSTALL_NPT,
        VirtualAddress,
        Handler,
        Trampoline,
        Kind);
}

UINT64
NoumenHypercallSvmTfStep(
    _In_ UINT64 Key
    )
{
    return noumen_vmcall(Key, NOUMEN_HYPERCALL_SVM_TF_STEP, 0, 0);
}

UINT64
NoumenHypercallHookRemovePage(
    _In_ UINT64 Key,
    _In_ UINT64 PagePhysicalAddress
    )
{
    return noumen_vmcall(Key, NOUMEN_HYPERCALL_HOOK_REMOVE_PAGE, PagePhysicalAddress & ~0xFFFULL, 0);
}
UINT64
NoumenHypercallHookApply(
    _In_ UINT64 Key
    )
{
    return noumen_vmcall(Key, NOUMEN_HYPERCALL_HOOK_APPLY, 0, 0);
}

UINT64
NoumenHypercallHookRetired(
    _In_ UINT64 Key
    )
{
    return noumen_vmcall(Key, NOUMEN_HYPERCALL_HOOK_RETIRED, 0, 0);
}

UINT64
NoumenHypercallUserBreakpointCommit(
    _In_ UINT64 Key,
    _In_ UINT64 VirtualAddress
    )
{
    return noumen_vmcall(Key, NOUMEN_HYPERCALL_USER_BREAKPOINT_COMMIT, VirtualAddress, 0);
}

UINT64
NoumenHypercallHideSelf(
    _In_ UINT64 Key
) {
    return noumen_vmcall(Key, NOUMEN_HYPERCALL_HIDE_SELF, 0, 0);
}