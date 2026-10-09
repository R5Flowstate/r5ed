//! Optional data parallelism. `rayon` is behind the default-on `parallel`
//! feature and is never compiled for `wasm32`, which has no threads here.

/// Map `f` over `0..n`, in parallel when the feature and target allow it.
#[inline]
pub fn for_each_index<F>(n: usize, f: F)
where
    F: Fn(usize) + Sync + Send,
{
    #[cfg(all(feature = "parallel", not(target_arch = "wasm32")))]
    {
        use rayon::prelude::*;
        (0..n).into_par_iter().for_each(f);
    }
    #[cfg(not(all(feature = "parallel", not(target_arch = "wasm32"))))]
    {
        for i in 0..n {
            f(i);
        }
    }
}

/// Apply `f` to every pair of a mutable slice and a read-only one, in parallel
/// when the feature and target allow it.
#[inline]
pub fn zip_mut<T, U, F>(a: &mut [T], b: &[U], f: F)
where
    T: Send,
    U: Sync,
    F: Fn(&mut T, &U) + Sync + Send,
{
    let n = a.len().min(b.len());
    #[cfg(all(feature = "parallel", not(target_arch = "wasm32")))]
    {
        use rayon::prelude::*;
        a[..n].par_iter_mut().zip(b[..n].par_iter()).for_each(|(x, y)| f(x, y));
    }
    #[cfg(not(all(feature = "parallel", not(target_arch = "wasm32"))))]
    {
        a[..n].iter_mut().zip(b[..n].iter()).for_each(|(x, y)| f(x, y));
    }
}

/// Apply `f` to every element in place, in parallel when the feature and
/// target allow it. The element is handed to the closure, so `f` stays a plain
/// `Fn` and the calls cannot alias.
#[inline]
pub fn for_each_mut<T, F>(values: &mut [T], f: F)
where
    T: Send,
    F: Fn(&mut T) + Sync + Send,
{
    #[cfg(all(feature = "parallel", not(target_arch = "wasm32")))]
    {
        use rayon::prelude::*;
        values.par_iter_mut().for_each(f);
    }
    #[cfg(not(all(feature = "parallel", not(target_arch = "wasm32"))))]
    {
        values.iter_mut().for_each(f);
    }
}

/// Map `f` over `0..n` collecting results, preserving index order either way.
#[inline]
pub fn map_indexed<T, F>(n: usize, f: F) -> Vec<T>
where
    T: Send,
    F: Fn(usize) -> T + Sync + Send,
{
    #[cfg(all(feature = "parallel", not(target_arch = "wasm32")))]
    {
        use rayon::prelude::*;
        (0..n).into_par_iter().map(f).collect()
    }
    #[cfg(not(all(feature = "parallel", not(target_arch = "wasm32"))))]
    {
        (0..n).map(f).collect()
    }
}

/// True when work is actually spread across threads.
pub const fn is_parallel() -> bool {
    cfg!(all(feature = "parallel", not(target_arch = "wasm32")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn for_each_visits_every_index() {
        let n = 1000;
        let seen = std::sync::atomic::AtomicUsize::new(0);
        for_each_index(n, |_| {
            seen.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        });
        assert_eq!(seen.load(std::sync::atomic::Ordering::Relaxed), n);
    }

    #[test]
    fn map_preserves_order() {
        let v = map_indexed(64, |i| i * i);
        assert_eq!(v.len(), 64);
        for (i, x) in v.iter().enumerate() {
            assert_eq!(*x, i * i);
        }
    }

    #[test]
    fn parallel_flag_is_consistent() {
        // The flag is a compile-time constant, so just assert it is readable.
        let _ = is_parallel();
    }
}
