#include "driver.h"

#define NOUMEN_ELF_MAX_FILE_SIZE  (64 * 1024 * 1024)
#define NOUMEN_ELF_MAX_IMAGE_SIZE (256 * 1024 * 1024)

static
BOOLEAN
NoumenElfRangeValid(
    _In_ ULONG64 TotalSize,
    _In_ ULONG64 Offset,
    _In_ ULONG64 Length
    )
{
    return (Offset <= TotalSize) && (Length <= TotalSize - Offset);
}

static
BOOLEAN
NoumenElfNameEquals(
    _In_ const UCHAR *Name,
    _In_ ULONG64 Remaining,
    _In_ const char *Expected
    )
{
    while (*Expected != '\0') {
        if (Remaining == 0 || *Name != (UCHAR)*Expected) {
            return FALSE;
        }

        Name++;
        Expected++;
        Remaining--;
    }

    return Remaining != 0 && *Name == '\0';
}

NTSTATUS
NoumenElfLoadFromFile(
    _In_ PCUNICODE_STRING Path,
            _In_ ULONG PhysMapSize,
    _Out_ PNOUMEN_ELF_IMAGE Image
    )
{
    HANDLE file = NULL;
    OBJECT_ATTRIBUTES objectAttributes;
    IO_STATUS_BLOCK ioStatus;
    FILE_STANDARD_INFORMATION fileInformation;
    NTSTATUS status;
    PVOID data = NULL;
    ULONG size;
    ULONG offset;

    if (Path == NULL || Image == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    RtlZeroMemory(Image, sizeof(*Image));

    InitializeObjectAttributes(
        &objectAttributes,
        (PUNICODE_STRING)Path,
        OBJ_CASE_INSENSITIVE | OBJ_KERNEL_HANDLE,
        NULL,
        NULL);

    status = ZwCreateFile(
        &file,
        GENERIC_READ | SYNCHRONIZE,
        &objectAttributes,
        &ioStatus,
        NULL,
        FILE_ATTRIBUTE_NORMAL,
        FILE_SHARE_READ,
        FILE_OPEN,
        FILE_SYNCHRONOUS_IO_NONALERT,
        NULL,
        0);

    if (!NT_SUCCESS(status)) {
        return status;
    }

    status = ZwQueryInformationFile(
        file,
        &ioStatus,
        &fileInformation,
        sizeof(fileInformation),
        FileStandardInformation);

    if (!NT_SUCCESS(status)) {
        ZwClose(file);
        return status;
    }

    if (fileInformation.EndOfFile.QuadPart <= 0 ||
        fileInformation.EndOfFile.QuadPart > (LONGLONG)NOUMEN_ELF_MAX_FILE_SIZE) {
        ZwClose(file);
        return STATUS_INVALID_IMAGE_FORMAT;
    }

    size = (ULONG)fileInformation.EndOfFile.QuadPart;
    data = ExAllocatePoolWithTag(NonPagedPoolNx, size, NOUMEN_POOL_TAG);
    if (data == NULL) {
        ZwClose(file);
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    status = STATUS_SUCCESS;
    offset = 0;

    while (offset < size) {
        status = ZwReadFile(
            file,
            NULL,
            NULL,
            NULL,
            &ioStatus,
            (PUCHAR)data + offset,
            size - offset,
            NULL,
            NULL);

        if (!NT_SUCCESS(status)) {
            break;
        }

        if (ioStatus.Information == 0) {
            status = STATUS_UNEXPECTED_IO_ERROR;
            break;
        }

        offset += (ULONG)ioStatus.Information;
    }

    if (NT_SUCCESS(status)) {
        status = NoumenElfLoadFromMemory(data, size, PhysMapSize, Image);
    }

    ExFreePoolWithTag(data, NOUMEN_POOL_TAG);
    ZwClose(file);

    return status;
}



/// 0                 loadSize                      symOffset             strOffset          totalSize
/// +---------------- + --------------------------- +-------------------- + ---------------- +
/// | ELF image       | pad                         | .symtab             | pad              | .strtab |
/// +---------------- + --------------------------- +-------------------- + ---------------- +
NTSTATUS
NoumenElfLoadFromMemory(
    _In_reads_bytes_(Size) const VOID *Data,
    _In_ ULONG Size,
            _In_ ULONG PhysMapSize,
    _Out_ PNOUMEN_ELF_IMAGE Image
    )
{
    const Elf64_Ehdr *ehdr;
    const Elf64_Phdr *phdr;
    const Elf64_Shdr *shdr;
    const Elf64_Shdr *poolShdr;
    const Elf64_Shdr *pfnShdr;
    const UCHAR *fileData;
    const UCHAR *shstr;
    const UCHAR *sectionName;
    Elf64_Addr minVaddr;
    Elf64_Addr maxVaddr;
    ULONG64 loadSize;
    ULONG64 relaVaddr;
    ULONG64 relaSize;
    ULONG64 initVaddr;
    ULONG64 relaOffset;
    ULONG64 relaCount;
    ULONG64 totalSize;
    ULONG64 tableOffset;
    ULONG64 imagePages;
    ULONG64 pfnBytes;
    ULONG64 nextTotal;
    ULONG64 nextPages;
#if NOUMEN_PANIC_DETAIL
    ULONG64 symSize;
    ULONG64 strSize;
    ULONG64 symEntrySize;
    ULONG64 sectionCount;
    ULONG64 symFileOffset;
    ULONG64 strFileOffset;
    ULONG64 symOffset;
    ULONG64 strOffset;
#endif
    ULONG64 shstrSize;
    ULONG64 nameRemaining;
    ULONG64 poolOffset;
    ULONG64 pfnOffset;
    ULONG64 poolPages;
    ULONG phnum;
    ULONG shnum;
#if NOUMEN_PANIC_DETAIL
    ULONG symIndex;
    ULONG strIndex;
#endif
    ULONG i;
    ULONG k;
    PUCHAR pool;
    UINT64 *pfns;
    UINT64 *imagePfns;
    PVOID image;
    PVOID raw;
    NTSTATUS status;
    BOOLEAN hasInit;

    if (Data == NULL || Image == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    RtlZeroMemory(Image, sizeof(*Image));

    poolShdr = NULL;
    pfnShdr = NULL;
    shstr = NULL;

    if (Size < sizeof(Elf64_Ehdr)) {
        return STATUS_INVALID_IMAGE_FORMAT;
    }

    ehdr = (const Elf64_Ehdr *)Data;
    fileData = (const UCHAR *)Data;

    if (ehdr->e_ident[EI_MAG0] != ELFMAG0 ||
        ehdr->e_ident[EI_MAG1] != ELFMAG1 ||
        ehdr->e_ident[EI_MAG2] != ELFMAG2 ||
        ehdr->e_ident[EI_MAG3] != ELFMAG3) {
        return STATUS_INVALID_IMAGE_FORMAT;
    }

    if (ehdr->e_ident[EI_CLASS] != ELFCLASS64 ||
        ehdr->e_ident[EI_DATA] != ELFDATA2LSB) {
        return STATUS_INVALID_IMAGE_FORMAT;
    }

    if (ehdr->e_machine != EM_X86_64 ||
        (ehdr->e_type != ET_EXEC && ehdr->e_type != ET_DYN)) {
        return STATUS_INVALID_IMAGE_FORMAT;
    }

    if (ehdr->e_phnum == 0 ||
        ehdr->e_phnum == PN_XNUM ||
        ehdr->e_phentsize != sizeof(Elf64_Phdr)) {
        return STATUS_INVALID_IMAGE_FORMAT;
    }

    if (!NoumenElfRangeValid(
            Size,
            ehdr->e_phoff,
            (ULONG64)ehdr->e_phnum * sizeof(Elf64_Phdr))) {
        return STATUS_INVALID_IMAGE_FORMAT;
    }

    phnum = ehdr->e_phnum;
    phdr = (const Elf64_Phdr *)(fileData + ehdr->e_phoff);

    minVaddr = MAXUINT64;
    maxVaddr = 0;

    for (i = 0; i < phnum; i++) {
        if (phdr[i].p_type != PT_LOAD) {
            continue;
        }

        if (phdr[i].p_filesz > phdr[i].p_memsz) {
            return STATUS_INVALID_IMAGE_FORMAT;
        }

        if (!NoumenElfRangeValid(Size, phdr[i].p_offset, phdr[i].p_filesz)) {
            return STATUS_INVALID_IMAGE_FORMAT;
        }

        if (phdr[i].p_vaddr > MAXUINT64 - phdr[i].p_memsz) {
            return STATUS_INVALID_IMAGE_FORMAT;
        }

        if (phdr[i].p_vaddr < minVaddr) {
            minVaddr = phdr[i].p_vaddr;
        }

        if (phdr[i].p_vaddr + phdr[i].p_memsz > maxVaddr) {
            maxVaddr = phdr[i].p_vaddr + phdr[i].p_memsz;
        }
    }

    if (minVaddr == MAXUINT64 || maxVaddr <= minVaddr) {
        return STATUS_INVALID_IMAGE_FORMAT;
    }

    if (maxVaddr - minVaddr > NOUMEN_ELF_MAX_IMAGE_SIZE) {
        return STATUS_INVALID_IMAGE_FORMAT;
    }

    if (ehdr->e_entry < minVaddr ||
        ehdr->e_entry - minVaddr >= maxVaddr - minVaddr) {
        return STATUS_INVALID_IMAGE_FORMAT;
    }

    loadSize = ALIGN_UP_BY(maxVaddr - minVaddr, PAGE_SIZE);

    if (ehdr->e_shoff == 0 ||
        ehdr->e_shnum == 0 ||
        ehdr->e_shentsize != sizeof(Elf64_Shdr) ||
        !NoumenElfRangeValid(
            Size,
            ehdr->e_shoff,
            (ULONG64)ehdr->e_shnum * sizeof(Elf64_Shdr))) {
        return STATUS_INVALID_IMAGE_FORMAT;
    }

    shdr = (const Elf64_Shdr *)(fileData + ehdr->e_shoff);
    shnum = ehdr->e_shnum;

    if (ehdr->e_shstrndx == SHN_UNDEF ||
        ehdr->e_shstrndx >= shnum ||
        shdr[ehdr->e_shstrndx].sh_type != SHT_STRTAB) {
        return STATUS_INVALID_IMAGE_FORMAT;
    }

    shstrSize = shdr[ehdr->e_shstrndx].sh_size;

    if (shstrSize == 0 ||
        shstrSize > NOUMEN_ELF_MAX_FILE_SIZE ||
        !NoumenElfRangeValid(Size, shdr[ehdr->e_shstrndx].sh_offset, shstrSize)) {
        return STATUS_INVALID_IMAGE_FORMAT;
    }

    shstr = fileData + shdr[ehdr->e_shstrndx].sh_offset;

    for (i = 0; i < shnum; i++) {
        if (shdr[i].sh_name >= shstrSize) {
            continue;
        }

        sectionName = shstr + shdr[i].sh_name;
        nameRemaining = shstrSize - shdr[i].sh_name;

        if (NoumenElfNameEquals(sectionName, nameRemaining, ".noumen.pool")) {
            poolShdr = &shdr[i];
        } else if (NoumenElfNameEquals(sectionName, nameRemaining, ".noumen.pfns")) {
            pfnShdr = &shdr[i];
        }
    }

    if (poolShdr == NULL ||
        pfnShdr == NULL ||
        poolShdr->sh_type != SHT_NOBITS ||
        pfnShdr->sh_type != SHT_NOBITS ||
        poolShdr->sh_size == 0 ||
        (poolShdr->sh_size % PAGE_SIZE) != 0 ||
        pfnShdr->sh_size != (poolShdr->sh_size / PAGE_SIZE) * sizeof(UINT64) ||
        poolShdr->sh_addr < minVaddr ||
        pfnShdr->sh_addr < minVaddr) {
        return STATUS_INVALID_IMAGE_FORMAT;
    }

    poolOffset = poolShdr->sh_addr - minVaddr;
    pfnOffset = pfnShdr->sh_addr - minVaddr;

    if (!NoumenElfRangeValid(loadSize, poolOffset, poolShdr->sh_size) ||
        !NoumenElfRangeValid(loadSize, pfnOffset, pfnShdr->sh_size) ||
        (poolOffset % PAGE_SIZE) != 0 ||
        (pfnOffset % sizeof(UINT64)) != 0) {
        return STATUS_INVALID_IMAGE_FORMAT;
    }

    poolPages = poolShdr->sh_size / PAGE_SIZE;
    totalSize = loadSize;

#if NOUMEN_PANIC_DETAIL
    symIndex = MAXULONG;
    sectionCount = 0;
    symSize = 0;
    strSize = 0;
    symOffset = 0;
    strOffset = 0;
    symFileOffset = 0;
    strFileOffset = 0;

    for (i = 0; i < shnum; i++) {
        if (shdr[i].sh_type == SHT_SYMTAB) {
            symIndex = i;
            break;
        }
    }

    if (symIndex != MAXULONG) {
        symSize = shdr[symIndex].sh_size;
        symEntrySize = shdr[symIndex].sh_entsize;
        strIndex = shdr[symIndex].sh_link;

        if (symEntrySize != sizeof(Elf64_Sym) ||
            symSize > NOUMEN_ELF_MAX_FILE_SIZE ||
            strIndex >= shnum ||
            shdr[strIndex].sh_type != SHT_STRTAB) {
            return STATUS_INVALID_IMAGE_FORMAT;
        }

        strSize = shdr[strIndex].sh_size;

        if (!NoumenElfRangeValid(Size, shdr[symIndex].sh_offset, symSize) ||
            !NoumenElfRangeValid(Size, shdr[strIndex].sh_offset, strSize) ||
            strSize > NOUMEN_ELF_MAX_FILE_SIZE) {
            return STATUS_INVALID_IMAGE_FORMAT;
        }

        sectionCount = symSize / symEntrySize;

        if (sectionCount != 0) {
            symFileOffset = shdr[symIndex].sh_offset;
            strFileOffset = shdr[strIndex].sh_offset;
            symOffset = ALIGN_UP_BY(loadSize, PAGE_SIZE);
            strOffset = ALIGN_UP_BY(symOffset + symSize, PAGE_SIZE);
            totalSize = strOffset + strSize;
        }
    }
#endif


    //
    if (PhysMapSize != 0) {
        ULONG64 physOffset = ALIGN_UP_BY(totalSize, sizeof(UINT64));

        totalSize = physOffset + PhysMapSize;
        Image->PhysMapOffset = (ULONG)physOffset;
        Image->PhysMapSize = PhysMapSize;
    }
    // 把“镜像所有页 PFN”表追加到镜像尾部，并让它落在
    // ImageBase..ImageBase+ImageSize 内。表本身也占页，所以页数需要迭代
    // 到稳定：imagePages = ceil((tableOffset + imagePages * 8) / PAGE_SIZE)。
    //
    tableOffset = ALIGN_UP_BY(totalSize, PAGE_SIZE);
    imagePages = tableOffset / PAGE_SIZE;

    for (i = 0; i < 8; i++) {
        pfnBytes = imagePages * sizeof(UINT64);
        nextTotal = tableOffset + pfnBytes;
        nextPages = ALIGN_UP_BY(nextTotal, PAGE_SIZE) / PAGE_SIZE;

        if (nextPages == imagePages) {
            break;
        }

        imagePages = nextPages;
    }

    totalSize = tableOffset + imagePages * sizeof(UINT64);

    //
    // 镜像按页对齐，PFN 表中的 page index 才能直接对应 ImageBase + i*PAGE；
    // 同时避免隐藏整页时连带同一页上的 pool 头。
    //
    raw = ExAllocatePoolWithTag(
        NonPagedPool,
        (SIZE_T)(totalSize + PAGE_SIZE),
        NOUMEN_POOL_TAG);

    if (raw == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    image = (PUCHAR)(((ULONG_PTR)raw + PAGE_SIZE - 1) & ~(ULONG_PTR)(PAGE_SIZE - 1));

    RtlZeroMemory(image, (SIZE_T)totalSize);

    Image->ImageRaw = raw;
    Image->ImageBase = image;
    Image->ImageSize = (ULONG)totalSize;
    Image->PfnTable = (PUCHAR)image + tableOffset;
    Image->PfnCount = (ULONG)imagePages;

    for (i = 0; i < phnum; i++) {
        if (phdr[i].p_type != PT_LOAD) {
            continue;
        }

        RtlCopyMemory(
            (PUCHAR)image + (phdr[i].p_vaddr - minVaddr),
            fileData + phdr[i].p_offset,
            (SIZE_T)phdr[i].p_filesz);
    }

    relaVaddr = 0;
    relaSize = 0;
    initVaddr = 0;
    hasInit = FALSE;

    for (i = 0; i < phnum; i++) {
        const Elf64_Dyn *dynamic;
        ULONG64 dynamicOffset;
        ULONG64 entryCount;
        ULONG64 n;

        if (phdr[i].p_type != PT_DYNAMIC) {
            continue;
        }

        if (!NoumenElfRangeValid(Size, phdr[i].p_offset, phdr[i].p_filesz) ||
            phdr[i].p_vaddr < minVaddr) {
            status = STATUS_INVALID_IMAGE_FORMAT;
            goto Fail;
        }

        dynamicOffset = phdr[i].p_vaddr - minVaddr;
        if (!NoumenElfRangeValid(loadSize, dynamicOffset, phdr[i].p_filesz)) {
            status = STATUS_INVALID_IMAGE_FORMAT;
            goto Fail;
        }

        dynamic = (const Elf64_Dyn *)((const UCHAR *)image + dynamicOffset);
        entryCount = phdr[i].p_filesz / sizeof(Elf64_Dyn);

        for (n = 0; n < entryCount; n++) {
            if (dynamic[n].d_tag == DT_NULL) {
                break;
            }

            if (dynamic[n].d_tag == DT_RELA) {
                relaVaddr = dynamic[n].d_un.d_ptr;
            } else if (dynamic[n].d_tag == DT_RELASZ) {
                relaSize = dynamic[n].d_un.d_val;
            } else if (dynamic[n].d_tag == DT_INIT) {
                initVaddr = dynamic[n].d_un.d_ptr;
                hasInit = TRUE;
            }
        }
    }

    if (relaSize != 0) {
        const Elf64_Rela *rela;

        if (relaVaddr < minVaddr ||
            (relaSize % sizeof(Elf64_Rela)) != 0) {
            status = STATUS_INVALID_IMAGE_FORMAT;
            goto Fail;
        }

        relaOffset = relaVaddr - minVaddr;
        if (!NoumenElfRangeValid(loadSize, relaOffset, relaSize)) {
            status = STATUS_INVALID_IMAGE_FORMAT;
            goto Fail;
        }

        rela = (const Elf64_Rela *)((const UCHAR *)image + relaOffset);
        relaCount = relaSize / sizeof(Elf64_Rela);

        for (k = 0; k < relaCount; k++) {
            ULONG64 targetOffset;

            if (ELF64_R_TYPE(rela[k].r_info) != R_X86_64_RELATIVE) {
                continue;
            }

            if (rela[k].r_offset < minVaddr ||
                rela[k].r_addend < 0 ||
                (Elf64_Addr)rela[k].r_addend < minVaddr) {
                status = STATUS_INVALID_IMAGE_FORMAT;
                goto Fail;
            }

            targetOffset = rela[k].r_offset - minVaddr;
            if (!NoumenElfRangeValid(loadSize, targetOffset, sizeof(UINT64))) {
                status = STATUS_INVALID_IMAGE_FORMAT;
                goto Fail;
            }

            *(UINT64 *)((PUCHAR)image + targetOffset) =
                (UINT64)((PUCHAR)image + (rela[k].r_addend - minVaddr));
        }
    }

    if (hasInit) {
        if (initVaddr < minVaddr || initVaddr - minVaddr >= loadSize) {
            status = STATUS_INVALID_IMAGE_FORMAT;
            goto Fail;
        }

        Image->InitFunction = (PUCHAR)image + (initVaddr - minVaddr);
    }

#if NOUMEN_PANIC_DETAIL
    if (sectionCount != 0) {
        Image->SymbolTable = (PUCHAR)image + symOffset;
        Image->SymbolTableSize = (ULONG)symSize;
        RtlCopyMemory(Image->SymbolTable, fileData + symFileOffset, (SIZE_T)symSize);
        Image->SymbolCount = (sectionCount > 0xffff) ? 0xffff : (USHORT)sectionCount;

        if (strSize != 0) {
            Image->StringTable = (PUCHAR)image + strOffset;
            Image->StringTableSize = (ULONG)strSize;
            RtlCopyMemory(Image->StringTable, fileData + strFileOffset, (SIZE_T)strSize);
        }
    }
#endif

    pool = (PUCHAR)image + poolOffset;
    pfns = (UINT64 *)((PUCHAR)image + pfnOffset);

    for (k = 0; k < poolPages; k++) {
        pfns[k] = MmGetPhysicalAddress(pool + ((ULONG64)k * PAGE_SIZE)).QuadPart >> PAGE_SHIFT;
    }

    //
    // 镜像所有页（包括这个 PFN 表所在的页）；HideSelf 会把它一起隐藏。
    //
    imagePfns = (UINT64 *)Image->PfnTable;

    for (k = 0; k < Image->PfnCount; k++) {
        imagePfns[k] = MmGetPhysicalAddress((PUCHAR)image + ((ULONG64)k * PAGE_SIZE)).QuadPart >> PAGE_SHIFT;
    }

    Image->EntryPoint = (PUCHAR)image + (ehdr->e_entry - minVaddr);

    return STATUS_SUCCESS;

Fail:
    NoumenElfFree(Image);
    return status;
}

VOID
NoumenElfFree(
    _Inout_ PNOUMEN_ELF_IMAGE Image
    )
{
    if (Image == NULL) {
        return;
    }

    if (Image->ImageRaw != NULL) {
        ExFreePoolWithTag(Image->ImageRaw, NOUMEN_POOL_TAG);
    } else if (Image->ImageBase != NULL) {
        ExFreePoolWithTag(Image->ImageBase, NOUMEN_POOL_TAG);
    }

    RtlZeroMemory(Image, sizeof(*Image));
}
