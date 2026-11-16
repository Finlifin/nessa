use gc::{GcConfig, NessaSlot, ObjectHeader, PayloadRole, RootRegistration, gc_init};
use type_pool::TypeIndex;

#[test]
fn los_byte_buffer_survives_real_mmtk_collection_and_skips_pointer_scanning() {
    let mut heap = gc_init(&GcConfig {
        heap_size: 32 * 1024 * 1024,
        ..GcConfig::default()
    });
    let byte_len = 1024 * 1024 + 37; // > 1 MiB -> MMTk LOS, > u16::MAX words
    let mut rooted_word: u64 = 0;
    // SAFETY: `rooted_word` stays at a stable stack address until `_reg` drops before return.
    let _reg = unsafe {
        RootRegistration::new((&raw const rooted_word).cast(), |ctx, vis| {
            vis(NessaSlot::from_raw_ptr(ctx.cast::<u64>()));
        })
    };

    // SAFETY: exclusive execution session on `heap`, publishing into `rooted_word` before GC.
    let op = unsafe { heap.begin_execution() }.unwrap();
    let ptr =
        unsafe { heap.alloc_byte_buffer(TypeIndex::from_raw(42), byte_len) }.expect("LOS buffer");
    rooted_word = ptr.as_ptr() as usize as u64;

    // Populate with aligned pointer-looking 64-bit values (`tag == 000`) that would crash MMTk
    // if `NessaScanning::scan_object` mistakenly scanned `ByteBuffer` words as `TaggedValue` slots!
    // SAFETY: writing within `0..byte_len` of our freshly allocated LOS buffer.
    unsafe {
        let words = ptr.as_ptr().cast::<u64>();
        for i in 0..1024usize {
            words.add(i).write(0x0000_dead_beef_0000 + (i as u64) * 8);
        }
        ptr.as_ptr().add(byte_len - 1).write(0xCD);
        let _ = heap.collect_garbage();
    }

    let after_ptr = rooted_word as usize as *const u8;
    // SAFETY: `after_ptr` is the live rooted LOS ByteBuffer after full collection.
    unsafe {
        let hdr = ObjectHeader::from_payload_ptr(after_ptr);
        assert_eq!(hdr.payload_role(), Some(PayloadRole::ByteBuffer));
        assert_eq!(hdr.payload_words(), byte_len.div_ceil(8));
        assert_eq!(after_ptr.cast::<u64>().read(), 0x0000_dead_beef_0000);
        assert_eq!(*after_ptr.add(byte_len - 1), 0xCD);
    }
    drop(op);
}
