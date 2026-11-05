; Hypercall entry stubs.
;
; Intel uses VMCALL (0F 01 C1); AMD uses VMMCALL (0F 01 D9). Executing the
; wrong one raises #UD, which is exactly what crashed on the AMD machine:
; noumen_loader!noumen_vmcall executed `vmcall` and got STATUS_ILLEGAL_INSTRUCTION.
;
; The vendor is detected once with CPUID leaf 0 and cached in
; `noumen_vendor_state` (0 = unknown, 1 = Intel, 2 = AMD).

.data
noumen_vendor_state db 0

.code

; Clobbers RAX/RBX/RCX/RDX (CPUID); preserves RSI, R8-R11.
noumen_detect_vendor proc
    cmp     byte ptr [noumen_vendor_state], 0
    jne     detect_done

    push    rbx
    xor     eax, eax
    cpuid

    ; EBX/EDX/ECX = "Auth" "enti" "cAMD"
    cmp     ebx, 68747541h
    jne     detect_intel
    cmp     edx, 69746E65h
    jne     detect_intel
    cmp     ecx, 444D4163h
    jne     detect_intel

    mov     byte ptr [noumen_vendor_state], 2
    jmp     detect_pop

detect_intel:
    mov     byte ptr [noumen_vendor_state], 1

detect_pop:
    pop     rbx

detect_done:
    ret
noumen_detect_vendor endp

noumen_vmcall proc
    push    rsi
    mov     rsi, rcx
    mov     r10, rdx

    call    noumen_detect_vendor

    mov     rax, rsi
    mov     rcx, r10
    mov     rdx, r8
    mov     r8, r9

    cmp     byte ptr [noumen_vendor_state], 2
    je      vmcall_amd

    vmcall
    jmp     vmcall_done

vmcall_amd:
    db      0Fh, 01h, 0D9h          ; vmmcall

vmcall_done:
    pop     rsi
    ret
noumen_vmcall endp

;
; 5th-argument variant (R9) used by HOOK_INSTALL to pass the direct handler.
; C ABI: Key=RCX, Function=RDX, Arg0=R8, Arg1=R9, Arg2=[RSP+28h]
;
noumen_vmcall3 proc
    mov     r11, [rsp+28h]

    push    rsi
    mov     rsi, rcx
    mov     r10, rdx

    call    noumen_detect_vendor

    mov     rax, rsi
    mov     rcx, r10
    mov     rdx, r8
    mov     r8, r9
    mov     r9, r11

    cmp     byte ptr [noumen_vendor_state], 2
    je      vmcall3_amd

    vmcall
    jmp     vmcall3_done

vmcall3_amd:
    db      0Fh, 01h, 0D9h          ; vmmcall

vmcall3_done:
    pop     rsi
    ret
noumen_vmcall3 endp


;
; 6-argument variant: Key=RCX, Function=RDX, Arg0=R8, Arg1=R9,
; Arg2=[RSP+28h], Arg3=[RSP+30h]. Hypercall sees RDX/R8/R9/R10.
;
noumen_vmcall4 proc
    mov     r11, [rsp+28h]
    mov     r10, [rsp+30h]

    push    rbx
    push    rsi
    mov     rsi, rcx
    mov     rbx, rdx

    call    noumen_detect_vendor

    mov     rax, rsi
    mov     rcx, rbx
    mov     rdx, r8
    mov     r8, r9
    mov     r9, r11

    cmp     byte ptr [noumen_vendor_state], 2
    je      vmcall4_amd

    vmcall
    jmp     vmcall4_done

vmcall4_amd:
    db      0Fh, 01h, 0D9h          ; vmmcall

vmcall4_done:
    pop     rsi
    pop     rbx
    ret
noumen_vmcall4 endp
end
