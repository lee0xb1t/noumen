#include "Driver.h"

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

	ULONG count = KeQueryActiveProcessorCount(NULL);

	for (int i = 0; i < count; i++) {
		KAFFINITY previousAffinity = KeSetSystemAffinityThreadEx((KAFFINITY)(1ULL << i));
		NoumenHypercallHideSelf(NOUMEN_HYPERCALL_KEY);
		KeRevertToUserAffinityThreadEx(previousAffinity);
	}

	return Status;
}