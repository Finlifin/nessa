//! Comprehensive integration tests for the unified lazy `Iterator` / `IntoIterator`
//! overhaul (`std.iter`), custom user iterators & scoped extensions, `!Error` and `#Effect`
//! callback propagation, static element provenance, and `std.stream` push-to-pull bridging.

mod common;

#[test]
fn iterator_adapters_work_across_all_builtin_collections_ranges_strings_and_bytes() {
    let result = common::run_value(
        r#"use std.iter

fn main() -> i64 {
    -- Range + reverse + step_by + map + filter + sum
    let r_sum = (1..=10)
        .rev()
        .filter(|n| n % 2 == 0)
        .map(|n| n * 10)
        .sum()
    assert_eq(r_sum, 300)

    -- List.iter() + enumerate + filter_map + collect (with static tuple binder narrowing!)
    let words = ["alpha", "", "beta", "gamma"]
    let tagged = List()
    for (idx, w) in words.iter().filter(|s| not s.is_empty()).enumerate() {
        let n: usize = idx
        let upper: String = w.to_uppercase()
        tagged.push("{n}:{upper}")
    }
    assert_eq(tagged, ["0:ALPHA", "1:BETA", "2:GAMMA"])

    -- zip + chain + take + skip + chunks + flatten
    let pairs = [10, 20, 30].iter().zip(["a", "b"]).collect()
    assert_eq(pairs.len(), 2)
    assert_eq(pairs(0), (10, "a"))
    assert_eq(pairs(1), (20, "b"))

    let chained = [1, 2].iter().chain(3..=5).skip(1).take(3).collect()
    assert_eq(chained, [2, 3, 4])

    let chunked = (1..=5).iter().chunks(2).collect()
    assert_eq(chunked, [[1, 2], [3, 4], [5]])

    let flat = [[1, 2], [], [3]].iter().flatten().product()
    assert_eq(flat, 6)

    -- Set, Deque, Map, HashMap, String, Bytes
    let s = Set.from_list([3, 1, 2, 3])
    assert_eq(s.iter().count(), (3).as(usize))

    let dq = Deque()
    dq.push_back(10)
    dq.push_front(5)
    assert_eq(dq.iter().collect(), [5, 10])

    let mp = Map()
    mp.set("x", 2)
    mp.set("y", 5)
    let m = mp.iter().map(|entry| {
        let (k, v) = entry
        "{k}={v}"
    }).to_set()
    assert(m.contains("x=2") and m.contains("y=5"))

    let rebuilt = [("k1", 11), ("k2", 22)].iter().to_map()
    assert_eq(rebuilt.get("k2"), 22)

    let hm = [("a", 1), ("b", 2)].iter().to_hash_map()
    assert_eq(hm.get("a"), 1)

    let ch_joined = "héllo".chars_iter().take(3).join(sep = "-")
    assert_eq(ch_joined, "h-é-l")
    assert_eq("a\nb\r\nc".lines_iter().collect(), ["a", "b", "c"])
    assert_eq(Bytes.from_list([1, 2, 3]).iter().map(|b| b.as(i64) * 2).sum(), 12)

    -- Peekable + generators (`once`, `repeat`, `from_fn`, `successors`, `unfold`)
    let pk = [9, 8, 7].iter().peekable()
    assert_eq(pk.peek(), 9)
    assert_eq(pk.peek(), 9)
    assert_eq(pk.collect(), [9, 8, 7])

    let p2 = iter.successors(1, |n| if n < 8 { n * 2 } else { null }).collect()
    assert_eq(p2, [1, 2, 4, 8])

    let fib = iter.unfold((0, 1), |st| {
        let (a, b) = st.as((i64, i64))
        if a > 8 { null } else { (a, (b, a + b)) }
    }).collect()
    assert_eq(fib, [0, 1, 1, 2, 3, 5, 8])

    assert_eq(iter.once(7).chain(iter.repeat(3).take(2)).sum(), 13)
    42
}
"#,
    );
    assert_eq!(result, Ok(42));
}

#[test]
fn custom_struct_impl_iterator_and_fallible_effectful_callbacks_short_circuit_cleanly() {
    let result = common::run_value(
        r#"struct CountUp {
    cur: i64,
    stop: i64,
}

impl Iterator for CountUp {
    assoc Item: Type = i64

    pub fn next(self) -> IterationStep(i64) {
        if self.cur >= self.stop {
            return IterationStep(i64).done
        }
        let out = self.cur
        self.cur += 1
        IterationStep(i64).yielded(out)
    }
}

enum CheckError {
    NegativeFound(val: i64),
}

effect audit_tick(val: i64) -> Unit

fn sum_checked(xs: List) -> !CheckError i64 {
    xs.iter().fold(0, |acc, x: Any| -> !CheckError i64 {
        let n = x.as(i64)
        if n < 0 {
            return error CheckError.NegativeFound(n)
        }
        acc + n
    })
}

fn main() -> i64 {
    -- Custom `CountUp` automatically gets `.filter`, `.map`, `.take`, `.fold`, `.first`, `.last`!
    let custom_total: i64 = CountUp { cur: 1, stop: 8 }
        .filter(|n| n % 2 == 1)
        .map(|n| n * n)
        .fold(0, |acc, v| acc + v)
    assert_eq(custom_total, 84)

    let ok_val = sum_checked([4, 5, 6]) ! {
        CheckError.NegativeFound(_) => -1,
    }
    assert_eq(ok_val, 15)

    let bad_val = sum_checked([4, -9, 100]) ! {
        CheckError.NegativeFound(v) => v,
    }
    assert_eq(bad_val, -9)

    -- Algebraic `#audit_tick` effect propagates cleanly through `Iterator.for_each`!
    let seen = List()
    let run_ticks = || -> #audit_tick Unit {
        [10, 20, 30].iter().for_each(|x| -> #audit_tick Unit {
            audit_tick(x)#
        })#
    }
    run_ticks()# {
        audit_tick(v) => {
            seen.push(v)
        },
    }
    assert_eq(seen.iter().sum(), 60)
    custom_total
}
"#,
    );
    assert_eq!(result, Ok(84));
}

#[test]
fn std_stream_effect_generators_and_lazy_to_iter_bridge_suspend_without_overrun() {
    let result = common::run_value(
        r#"use std.stream

fn infinite_multiples(step: i64, counter: List) -> #stream.emit Unit {
    var k = 1
    while true {
        counter.push(k)
        stream.emit(k * step)#
        k += 1
    }
}

fn main() -> i64 {
    let collected = stream.collect(|| {
        stream.emit("a")#
        stream.emit("b")#
        stream.emit("c")#
    })
    assert_eq(collected, ["a", "b", "c"])

    let folded: i64 = stream.fold(
        || {
            stream.from_iter(1..=4)#
        },
        10,
        |acc, v| acc + v.as(i64),
    )
    assert_eq(folded, 20)

    -- Verify `stream.to_iter` suspends via delimited continuation and stops after 4 pulls!
    let pulls = List()
    let first_four = stream.to_iter(|| infinite_multiples(5, pulls)#)
        .take(4)
        .collect()
    assert_eq(first_four, [5, 10, 15, 20])
    assert_eq(pulls.len(), 4)
    100
}
"#,
    );
    assert_eq!(result, Ok(100));
}
