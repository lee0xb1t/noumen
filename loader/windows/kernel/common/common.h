#pragma once

// ntifs.h 必须最先包含：它带着 _NTIFS_INCLUDED_ 去包含 ntddk.h，使
// PEPROCESS/PETHREAD 成为进程 API 期望的 KPROCESS/KTHREAD 类型。
#include <ntifs.h>
#include <ntstrsafe.h>
#include <intrin.h>

#include "Public.h"
#include "command.h"
#include "elf.h"
#include "elfloader.h"
#include "hypercall.h"
#include "hypervisor.h"
#include "noumenlog.h"
#include "path.h"
