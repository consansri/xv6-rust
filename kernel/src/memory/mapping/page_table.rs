use core::ptr::{write, read, write_bytes, copy_nonoverlapping};
use crate::{interrupt::trap::kerneltrap, println, register::{sfence_vma, satp}};
use crate::memory::mapping::page_table_entry::{ PageTableEntry, PteFlags};
use crate::define::memlayout::{ PGSIZE, MAXVA, PGSHIFT };
use crate::memory::{
    address::{ VirtualAddress, PhysicalAddress, Addr }, 
    kalloc:: {kalloc, kfree}, 
    container::boxed::Box,
};
use super::*;

#[derive(Debug, Clone, Copy)]
#[repr(C, align(4096))]
pub struct PageTable{
    pub entries: [PageTableEntry; PGSIZE/8],
}

static mut KERNEL_PAGETABLE:PageTable = PageTable::empty();


impl PageTable{
    pub fn as_addr(&self) -> usize{
        self.entries.as_ptr() as usize
    }

    pub const fn empty() -> Self{
        Self{
            entries:[PageTableEntry(0); PGSIZE/8]
        }
    }

    /// Convert the page table to be the usize
    /// that can be written in satp register
    pub fn as_satp(&self) -> usize{
        satp::SATP_SV39 | ((self.entries.as_ptr() as usize) >> PGSHIFT)
    }

    #[inline]
    pub fn clear(&mut self){
        for pte in self.entries.iter_mut(){
            pte.write_zero();
        }
    }

    

    // Return the address of the PTE in page table pagetable
    // that corresponds to virtual address va.  If alloc!=0,
    // create any required page-table pages.
    //
    // The risc-v Sv39 scheme has three levels of page-table
    // pages. A page-table page contains 512 64-bit PTEs.
    // A 64-bit virtual address is split into five fields:
    //   39..63 -- must be zero.
    //   30..38 -- 9 bits of level-2 index.
    //   21..29 -- 9 bits of level-1 index.
    //   12..20 -- 9 bits of level-0 index.
    //    0..11 -- 12 bits of byte offset within the page.


    // find  the PTE for a virtual address
     fn walk(&mut self, va: VirtualAddress, alloc:i32) -> Option<&mut PageTableEntry>{
        let mut pagetable = self as *mut PageTable;
        let real_addr:usize = va.as_usize();
        if real_addr > MAXVA {
            panic!("walk");
        }
        for level in (1..=2).rev() {
            let pte = unsafe{ &mut (*pagetable).entries[va.page_num(level)] };
            if pte.is_valid() {
                pagetable = pte.as_pagetable();
    
            }else{
                if alloc == 0{
                    return None
                }
                match unsafe{Box::<PageTable>::new()}{
                    Some(mut new_pagetable) => {
                        // let page_addr = page_table as usize;
                        // for i in 0..PGSIZE{
                        //     unsafe{write((page_addr + i) as *mut u8, 0)};
                        // }
                        // unsafe{write((pte as *const _) as *mut PageTableEntry, PageTableEntry::as_pte(page_addr).add_valid_bit())};
                        new_pagetable.clear();
                        pagetable = new_pagetable.into_raw();
                        pte.0 = (((pagetable as usize) >> 12) << 10) | (PteFlags::V.bits());

                        
                    }
                    None => return None,
                }
                
            }
        }
        Some(unsafe{&mut (*pagetable).entries[va.page_num(0)]})
    }

    // Look up a virtual address, return the physical address,
    // or 0 if not mapped.
    // Can only be used to look up user pages.
    pub fn walkaddr(pagetable: &mut PageTable, va: VirtualAddress) -> Option<PhysicalAddress>{
        let addr = va.as_usize();
        if addr > MAXVA{
            return None
        }
        match pagetable.walk(va, 0){
            Some(pte) => {
                if !pte.is_valid(){
                    return None
                }
                if !pte.is_user(){
                    return None
                }

                let pagetable_addr = pte.as_pagetable() as usize;
                Some(PhysicalAddress::new(pagetable_addr))
            }

            None => None
        }
    }


    // Create PTEs for virtual addresses starting at va that refer to
    // physical addresses starting at pa. va and size might not
    // be page-aligned. Returns 0 on success, -1 if walk() couldn't
    // allocate a needed page-table page.

    unsafe fn mappages(
        &mut self, 
        mut va: VirtualAddress, 
        mut pa: PhysicalAddress, 
        size:usize, 
        perm:PteFlags
    ) -> bool{
        // let mut start:VirtualAddress = VirtualAddress::new(va.page_round_down());
        // let mut end:VirtualAddress = VirtualAddress::new(va.add_addr(size -1).page_round_down());
        let mut last = VirtualAddress::new(va.as_usize() + size);
        va.pg_round_down();
        last.pg_round_up();
        while va != last{
            match self.walk(va, 1){
                Some(pte) => {
                // TODO - is_valid?
                if pte.is_valid(){
                    println!(
                        "va: {:#x}, pa: {:#x}, pte: {:#x}",
                        va.as_usize(),
                        pa.as_usize(),
                        pte.0
                    );
                    panic!("remap");
                }
                // let pa_usize = pa.as_usize();
                //  *pte = PageTableEntry::new(PageTableEntry::as_pte(pa_num).as_usize() | perm).add_valid_bit();
                 
                // write(pte.as_mut_ptr() as *mut PageTableEntry,
                // PageTableEntry::new(PageTableEntry::as_pte(pa_usize).as_usize() | perm).add_valid_bit());
                //  println!("write pagetable entry");
                pte.write_perm(pa, perm);
                va.add_page();
                pa.add_page();

                }
                None => return false
             }
        }
        true
    }

    // add a mapping to the kernel page table.
    // only used when booting
    // does not flush TLB or enable paging
    
    pub unsafe fn kvmmap(&mut self, 
        va:VirtualAddress, 
        pa:PhysicalAddress, 
        size:usize, 
        perm:PteFlags){
        println!(
            "kvm_map: va={:#x}, pa={:#x}, size={:#x}",
            va.as_usize(),
            pa.as_usize(),
            size
        );
        if !self.mappages(va, pa, size, perm){
            panic!("kvmmap");
        }
    }


    // Create an empty user page table.
    // return None if out of memory
    unsafe fn uvmcreate() -> Option<*mut PageTable>{
        match Box::<PageTable>::new(){
            Some(mut page_table) => {
                page_table.clear();
                Some(&mut (*page_table))
            }

            None => None
        }
    }

    // Load the user initcode into address 0 of pagetable
    // for the very first process
    // sz must be less than a page

    pub unsafe fn uvminit(&mut self, src: *const u8, size:usize){
        if size >= PGSIZE{
            panic!("inituvm: more than a page");
        }

        if let Some(mem) = kalloc(){
            write_bytes(mem, 0, PGSIZE);

            self.mappages(
                VirtualAddress::new(0), 
                PhysicalAddress::new(mem as usize), 
                PGSIZE, 
                PteFlags::W | PteFlags::R | PteFlags::X | PteFlags::U
            );

            copy_nonoverlapping(src, mem, PGSIZE);
        }
    }


    // Allocate PTEs and physical memory to grow process from oldsz to
    // newsz, which need not be page aligned.  Returns new size or 0 on error.
    pub unsafe fn uvmalloc(&mut self, mut old_size:usize, new_size:usize) -> Option<usize>{
        if new_size < old_size {
            return Some(old_size)
        }

        old_size = page_round_up(old_size);
        let mut a = old_size;
        while a < new_size {
            match kalloc(){
                Some(mem) => {
                
                    write_bytes(mem, 0, PGSIZE);
                    
                    if self.mappages(
                        VirtualAddress::new(a), 
                        PhysicalAddress::new(mem as usize), 
                        PGSIZE, 
                        PteFlags::W | PteFlags::R | PteFlags::X | PteFlags::U
                    ){
                        kfree(PhysicalAddress::new(mem as usize));
                        self.uvmdealloc(a, old_size);
                        return None
                    }
                }

                None => {
                    self.uvmdealloc(a, old_size);
                    return None
                }
            }

            a += PGSIZE;
        }

        Some(new_size)
    }


    // Deallocate user pages to bring the process size from oldsz to
    // newsz.  oldsz and newsz need not be page-aligned, nor does newsz
    // need to be less than oldsz.  oldsz can be larger than the actual
    // process size.  Returns the new process size.

    pub fn uvmdealloc(&mut self, old_size:usize, new_size:usize) -> usize{
        if new_size >= old_size {
            return old_size
        }

        if page_round_up(new_size) < page_round_up(old_size){
            let npages = (page_round_up(old_size) - page_round_up(new_size)) / PGSIZE;
            self.uvmunmap(
                VirtualAddress::new(page_round_up(new_size)), 
                npages, 
                1
            );
        }

        new_size

    }


    // Remove npages of mappings starting from va. va must be
    // page-aligned. The mappings must exist.
    // Optionally free the physical memory.
    pub fn uvmunmap(&mut self, va:VirtualAddress, npages:usize, do_free:usize){
        if !va.is_page_aligned(){
            panic!("uvmunmap: not aligned");
        }

        let mut a = va.clone();

        while a != va.add_addr(npages * PGSIZE){
            match self.walk(va, 0){
                Some(pte) => {
                    if pte.as_usize() & PteFlags::V.bits() == 0 {
                        panic!("uvmunmap: not mapped")
                    }

                    if pte.as_flags() == PteFlags::V.bits() {
                        panic!("uvmunmap: not a leaf")
                    }

                    if do_free != 0 {
                        unsafe{
                            let pa = (&(*pte.as_pagetable())).as_addr();
                            kfree(PhysicalAddress::new(pa));
                        }
                    }

                    pte.write_zero();
                }

                None => panic!("uvmunmap: walk")
            }

            a.add_page()
        }


    }

}