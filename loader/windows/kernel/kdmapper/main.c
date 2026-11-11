#include "Driver.h"

//
// 这两个状态是 hypervisor 全局状态，不是 per-CPU 状态。放在 HideSelf 的亲和性
// 循环外调用一次即可；放在循环内只会重复覆盖同一份缓存。
//
static
VOID
SetPhysMemoryRange(
    VOID
    )
{
    PHYSICAL_MEMORY_RANGE *ranges = MmGetPhysicalMemoryRanges();
    ULONG count = 0;

    if (ranges == NULL) {
        return;
    }

    while (count < NOUMEN_MAX_PHYS_RANGES &&
           ranges[count].NumberOfBytes.QuadPart != 0) {
        count++;
    }

    if (count != 0) {
        NoumenHypercallSetPhysMemoryRange(
            NOUMEN_HYPERCALL_KEY,
            (UINT64)(ULONG_PTR)ranges,
            (UINT64)count);
    }

    ExFreePool(ranges);
}

static
VOID
SetSystemCr3(
    VOID
    )
{
    UINT64 cr3 = (UINT64)g_NoumenHypervisor.KernelArgs.SystemCr3;

    if (cr3 != 0) {
        NoumenHypercallSetSystemCr3(NOUMEN_HYPERCALL_KEY, cr3);
    }
}

NTSTATUS CustomDriverEntry(
	_In_ PDRIVER_OBJECT  kdmapperParam1,
	_In_ PUNICODE_STRING kdmapperParam2
)
{
	UNREFERENCED_PARAMETER(kdmapperParam1);
	UNREFERENCED_PARAMETER(kdmapperParam2);

	NTSTATUS Status = STATUS_SUCCESS;

	NoumenHypervisorInitialize();

	//Status = NoumenLogInitialize();
	//if (!NT_SUCCESS(Status)) {
	//	return Status;
	//}

	Status = NoumenHypervisorLoadImageFromMemory(noumen_data, sizeof(noumen_data));

	if (!NT_SUCCESS(Status)) {
		return Status;
	}

	Status = NoumenHypervisorStart(FALSE, FALSE);

	if (!NT_SUCCESS(Status)) {
		NoumenHypervisorCleanup();
		return Status;
	}

	SetSystemCr3();
	SetPhysMemoryRange();

	ULONG count = KeQueryActiveProcessorCount(NULL);
	ULONG i;

	for (i = 0; i < count; i++) {
		KAFFINITY previousAffinity = KeSetSystemAffinityThreadEx((KAFFINITY)(1ULL << i));
		NoumenHypercallHideSelf(NOUMEN_HYPERCALL_KEY);
		KeRevertToUserAffinityThreadEx(previousAffinity);
	}

	return Status;
}