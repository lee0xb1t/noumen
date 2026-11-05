#pragma once

// ntifs.h 必须最先包含：它带着 _NTIFS_INCLUDED_ 去包含 ntddk.h，使
// PEPROCESS/PETHREAD 成为进程 API 期望的 KPROCESS/KTHREAD 类型。
#include <ntifs.h>
#include <ntstrsafe.h>
#include <intrin.h>

#include "public.h"
#include "elf.h"
#include "elfloader.h"
#include "path.h"
#include "hypercall.h"
#include "hypervisor.h"
#include "noumenlog.h"
#include "command.h"
#include "noumen_data.h"

DRIVER_INITIALIZE DriverEntry;
DRIVER_UNLOAD NoumenEvtDriverUnload;

