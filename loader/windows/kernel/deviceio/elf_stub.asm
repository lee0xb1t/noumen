.code

call_elf2 proc
	push rdi
	push rsi
	sub rsp, 8
	mov rax, rcx
	mov rdi, rdx
	mov rsi, r8
	call rax
	add rsp, 8
	pop rsi
	pop rdi
	ret
call_elf2 endp

end
