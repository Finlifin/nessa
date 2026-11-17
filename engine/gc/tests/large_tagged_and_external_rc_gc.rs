use gc::{
    ExternalRcHandle, GcConfig, NessaSlot, ObjectHeader, PayloadRole, RootRegistration,
    external_live_bytes, external_rc_bytes, external_shell_count, gc_init,
};
use type_pool::{Intrinsic, TypeIndex};

#[test]
fn large_tagged_and_external_rc_survive_real_mmtk_collection_and_sweep_dead_buffers() {
    let mut heap = gc_init(&GcConfig {
        heap_size: 64 * 1024 * 1024,
        ..GcConfig::default()
    });

    // Root table: [0] = LargeTagged array (`70_000` words > `u16::MAX`),
    //             [1] = surviving ExternalRc string shell (`2 MiB`),
    //             [2] = temporary ExternalRc shell dropped before GC (`3 MiB`).
    let mut roots: [u64; 3] = [0, 0, 0];
    // SAFETY: `roots` remains at a stable stack address until `_reg` drops before return.
    let _reg = unsafe {
        RootRegistration::new((&raw const roots).cast(), |ctx, vis| {
            let base = ctx.cast::<u64>();
            for i in 0..3usize {
                vis(NessaSlot::from_raw_ptr(base.add(i)));
            }
        })
    };

    // SAFETY: exclusive execution session on `heap`; every allocated object is rooted in `roots`
    // or inside the rooted `LargeTagged` container before the next allocation or GC.
    let op = unsafe { heap.begin_execution() }.unwrap();

    let large_words = (u16::MAX as usize) + 4_465; // 70,000 slots
    let large_ptr = unsafe { heap.alloc_tagged_words(TypeIndex::from_raw(500), large_words) }
        .expect("LargeTagged LOS allocation");
    roots[0] = large_ptr.as_ptr() as usize as u64;

    let initial_stripe =
        unsafe { ObjectHeader::from_payload_ptr(large_ptr.as_ptr()).stripe_index() };
    assert!(initial_stripe < 64);

    // Populate first & last slots of the 70,000-word `LargeTagged` object with live Immix child
    // strings, and middle slots with immediate scalar `TaggedValue` tags (`0x01` small ints).
    unsafe {
        let base = (roots[0] as usize) as *mut u64;
        for i in 0..large_words {
            base.add(i).write(((i as u64) << 7) | 0x01);
        }

        let child_a = heap
            .alloc_object(Intrinsic::Str.type_index(), 2)
            .expect("child_a");
        *(child_a.as_ptr() as *mut u64) = 5;
        std::ptr::copy_nonoverlapping(b"alpha".as_ptr(), child_a.as_ptr().add(8), 5);
        ((roots[0] as usize) as *mut u64).write(child_a.as_ptr() as usize as u64);

        let child_b = heap
            .alloc_object(Intrinsic::Str.type_index(), 2)
            .expect("child_b");
        *(child_b.as_ptr() as *mut u64) = 5;
        std::ptr::copy_nonoverlapping(b"omega".as_ptr(), child_b.as_ptr().add(8), 5);
        ((roots[0] as usize) as *mut u64)
            .add(large_words - 1)
            .write(child_b.as_ptr() as usize as u64);
    }

    // Allocate two ExternalRc shells: a 2 MiB survivor (`roots[1]`) and a 3 MiB ephemeral (`roots[2]`).
    let live_len = 2 * 1024 * 1024;
    let dead_len = 3 * 1024 * 1024;
    let live_buf = vec![b'L'; live_len];
    let dead_buf = vec![b'D'; dead_len];

    let live_handle = ExternalRcHandle::from_slice(&live_buf).expect("live ExternalRcHandle");
    let live_shell =
        unsafe { heap.alloc_external_rc(Intrinsic::Str.type_index(), live_handle, 0, live_len) }
            .expect("live ExternalRc shell");
    roots[1] = live_shell.as_ptr() as usize as u64;

    let dead_handle = ExternalRcHandle::from_slice(&dead_buf).expect("dead ExternalRcHandle");
    let dead_shell =
        unsafe { heap.alloc_external_rc(Intrinsic::Str.type_index(), dead_handle, 0, dead_len) }
            .expect("dead ExternalRc shell");
    roots[2] = dead_shell.as_ptr() as usize as u64;

    assert!(external_live_bytes() >= live_len + dead_len);
    assert!(external_shell_count() >= 2);

    // Drop the 3 MiB shell from roots and trigger two full collections!
    unsafe {
        (&raw mut roots[2]).write_volatile(0);
        assert!(heap.collect_garbage());
        assert!(heap.collect_garbage());
    }

    // Dead 3 MiB ExternalRc buffer MUST be swept and reclaimed; live 2 MiB buffer MUST survive!
    assert_eq!(external_live_bytes(), live_len);
    assert_eq!(external_shell_count(), 1);

    unsafe {
        let after_large = roots[0] as usize as *const u8;
        let hdr = ObjectHeader::from_payload_ptr(after_large);
        assert_eq!(hdr.payload_role(), Some(PayloadRole::LargeTagged));
        assert!(hdr.is_ordinary());
        assert_eq!(hdr.payload_words(), large_words);
        assert_eq!(hdr.stripe_index(), initial_stripe);

        let slot0 = *(after_large as *const u64) as usize as *const u8;
        let slot_last = *((after_large as *const u64).add(large_words - 1)) as usize as *const u8;
        assert_eq!(
            gc::heap_string_bytes(slot0),
            Some(b"alpha".as_slice()),
            "slot 0 child survived GC via LargeTagged scanning"
        );
        assert_eq!(
            gc::heap_string_bytes(slot_last),
            Some(b"omega".as_slice()),
            "last slot (69,999) child survived GC via LargeTagged scanning"
        );

        let after_ext = roots[1] as usize as *const u8;
        let ext_hdr = ObjectHeader::from_payload_ptr(after_ext);
        assert_eq!(ext_hdr.payload_role(), Some(PayloadRole::ExternalRc));
        let slice = external_rc_bytes(after_ext).expect("live ExternalRc bytes");
        assert_eq!(slice.len(), live_len);
        assert_eq!(slice[0], b'L');
        assert_eq!(slice[live_len - 1], b'L');
    }

    // Now drop the remaining 2 MiB survivor and verify external bytes return to 0!
    unsafe {
        (&raw mut roots[1]).write_volatile(0);
        assert!(heap.collect_garbage());
    }
    assert_eq!(external_live_bytes(), 0);
    assert_eq!(external_shell_count(), 0);

    drop(op);
}
