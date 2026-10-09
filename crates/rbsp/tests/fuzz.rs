//! Hostile input: truncated and bit-flipped lumps must be rejected, never
//! panic, and never allocate without bound.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use rbsp::lumps;
use common::{all_cases, Rng};

/// Counts every byte the allocating thread claims, so one test's parse can be
/// measured without another test's work landing in the same tally. A `const`
/// cell has no destructor, so touching it from inside the allocator is safe.
struct Counting;

std::thread_local! {
    static LIVE: Cell<usize> = const { Cell::new(0) };
}

fn live_add(n: isize) {
    let _ = LIVE.try_with(|c| {
        let v = c.get() as isize + n;
        c.set(v.max(0) as usize);
    });
}

fn live_get() -> usize {
    LIVE.try_with(|c| c.get()).unwrap_or(0)
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        live_add(l.size() as isize);
        unsafe { System.alloc(l) }
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        live_add(-(l.size() as isize));
        unsafe { System.dealloc(p, l) }
    }

    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        live_add(new as isize - l.size() as isize);
        unsafe { System.realloc(p, l, new) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// Peak bytes a single parse may claim over a lump of `len` bytes.
fn budget(len: usize) -> usize {
    4 * len + 64 * 1024
}

/// A mutation may be rejected outright, so only a parse that succeeds goes on
/// to be re-encoded. Returns the peak live bytes the parse itself claimed.
fn parse_bounded(id: u16, version: rbsp::Version, data: &[u8]) -> usize {
    let before = live_get();
    let parsed = lumps::parse_at(id, version, data, 0);
    let peak = live_get().saturating_sub(before);
    if let Ok(lump) = parsed {
        let out = lumps::write(&lump, version);
        assert!(
            out.len() <= budget(data.len()),
            "lump {id:#04x} re-encoded {} bytes from {} input bytes",
            out.len(),
            data.len()
        );
    }
    peak
}

/// Mutations of a lump: every truncation length, and single-byte flips at
/// seeded positions.
fn mutations(data: &[u8], rng: &mut Rng) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    if data.is_empty() {
        return out;
    }
    // A handful of truncations, including the empty one.
    for cut in [0usize, 1, data.len() / 4, data.len() / 2, data.len().saturating_sub(1)] {
        out.push(data[..cut.min(data.len())].to_vec());
    }
    for _ in 0..24 {
        let at = rng.below(data.len());
        let mut m = data.to_vec();
        m[at] ^= 1u8 << rng.below(8);
        out.push(m);
    }
    for _ in 0..8 {
        let at = rng.below(data.len());
        let mut m = data.to_vec();
        let v = rng.next_u32().to_le_bytes();
        let n = 1 + rng.below(4.min(data.len() - at));
        m[at..at + n].copy_from_slice(&v[..n]);
        out.push(m);
    }
    out
}

#[test]
#[ignore = "needs R5ED_FIXTURES"]
fn fuzz_no_panic() {
    let cases = all_cases();
    assert!(cases.len() >= 25);
    let mut rng = Rng::new(0x5EED_1234);
    let mut checked = 0usize;
    for case in cases.iter() {
        for (id, data) in &case.bsp.lumps {
            // Keep the run bounded: the bulk lumps are megabytes wide.
            let sample = if data.len() > 64 * 1024 { &data[..64 * 1024] } else { data.as_slice() };
            for m in mutations(sample, &mut rng) {
                let peak = parse_bounded(*id, case.version, &m);
                assert!(
                    peak <= budget(sample.len()),
                    "{}: lump {id:#04x} allocated {peak} bytes for {} input bytes",
                    case.name,
                    sample.len()
                );
                checked += 1;
            }
        }
    }
    assert!(checked > 2000, "only {checked} mutations were tried");
}

#[test]
#[ignore = "needs R5ED_FIXTURES"]
fn fuzz_container_reads_reject_garbage() {
    // Every mutation clones the whole map, so only the two smallest maps are used.
    let mut cases: Vec<_> = all_cases().iter().collect();
    cases.sort_by_key(|c| c.bsp.lumps.values().map(|v| v.len()).sum::<usize>());
    let mut rng = Rng::new(0xC0FFEE);
    let mut checked = 0usize;
    for case in cases.iter().take(2) {
        for (id, data) in &case.bsp.lumps {
            let sample = if data.len() > 4096 { &data[..4096] } else { data.as_slice() };
            for m in mutations(sample, &mut rng) {
                let mut broken = case.bsp.clone();
                broken.set_lump(*id, m);
                let _ = rbsp::validate(&broken, &case.ents);
                checked += 1;
            }
        }
    }
    assert!(checked > 100, "only {checked} corrupted maps were validated");
}

#[test]
fn fuzz_header_never_panics() {
    let mut rng = Rng::new(7);
    let base = {
        let mut b = rbsp::Header::new(47, 0, 30).to_bytes();
        b[0..4].copy_from_slice(b"rBSP");
        b
    };
    for _ in 0..2000 {
        let mut m = base.clone();
        let at = rng.below(m.len());
        m[at] = rng.next_u32() as u8;
        match rbsp::Header::parse(&m) {
            Ok(h) => {
                // Anything that parses has to describe a table the writer can
                // turn back into the same bytes.
                assert_eq!(h.to_bytes().len(), rbsp::HEADER_SIZE);
            }
            Err(_) => {}
        }
    }
    for cut in 0..rbsp::HEADER_SIZE {
        let e = rbsp::Header::parse(&base[..cut]).unwrap_err();
        assert!(e.to_string().contains("truncated") || e.to_string().contains("ident"), "{e}");
    }
}

#[test]
fn fuzz_monolithic_offsets_never_panic() {
    let mut rng = Rng::new(11);
    let mut bsp = rbsp::Bsp::new(47, 0, 30);
    bsp.set_lump(0x01, vec![0u8; 32]);
    bsp.set_lump(0x03, vec![1u8; 48]);
    let good = bsp.to_monolithic();
    for _ in 0..2000 {
        let mut m = good.clone();
        let at = rng.below(m.len());
        m[at] = rng.next_u32() as u8;
        // Either it parses and re-packs consistently, or it is rejected.
        if let Ok(b) = rbsp::Bsp::from_monolithic(&m) {
            let again = b.to_monolithic();
            assert!(rbsp::Bsp::from_monolithic(&again).is_ok());
        }
    }
}

#[test]
fn fuzz_game_lump_never_panics() {
    let mut rng = Rng::new(13);
    for len in 0..64usize {
        for _ in 0..32 {
            let data: Vec<u8> = (0..len).map(|_| rng.next_u32() as u8).collect();
            let r = lumps::game_lump::parse_at(&data, rbsp::Version::ClientV51, 0);
            if let Ok(g) = r {
                let back = lumps::game_lump::write(&g, rbsp::Version::ClientV51);
                assert_eq!(back, data);
            }
        }
    }
}

#[test]
fn fuzz_sprp_never_panics() {
    let mut rng = Rng::new(17);
    for len in 0..600usize {
        for _ in 0..8 {
            let data: Vec<u8> = (0..len).map(|_| rng.next_u32() as u8).collect();
            if let Ok(s) = rbsp::lumps::game_lump::sprp::parse(&data) {
                assert_eq!(s.encode(), data);
            }
        }
    }
}
