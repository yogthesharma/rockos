OUTPUT_FORMAT("elf64-x86-64")
ENTRY(_start)

/* Single LOAD that includes ELF + PHDR + sections so `e_phoff` is inside the file span. */
PHDRS
{
  all PT_LOAD FILEHDR PHDRS FLAGS(7);
}

SECTIONS
{
  . = 0x400000;

  .text : ALIGN(16) {
    *(.text._start .text .text.*)
  } : all

  .rodata : ALIGN(16) {
    *(.rodata .rodata.*)
  } : all

  .data : ALIGN(16) {
    *(.data .data.*)
  } : all

  .bss : ALIGN(16) {
    *(.bss .bss.*)
    *(COMMON)
  } : all

  /DISCARD/ : {
    *(.eh_frame .eh_frame_hdr)
    *(.note .note.*)
    *(.comment)
  }
}
