//! Comprehensive integration tests for Wave 4 standard library modules:
//! `std.fmt`, `std.pattern`, `std.string`, `std.bytes`, `std.math`, `std.rand`, and `std.scalar`.

mod common;

#[test]
fn fmt_specifiers_prelude_exports_and_typed_errors() {
    let source = r#"
        use std.fmt.{FormatError, try_sprintf, try_sprintf_list}

        fn main() -> i64 {
            if format("%d %i %u", -42, 42, 99) != "-42 42 99" { return 1 }
            if format("%+06d|%#x|%#X|%#o|%b", 42, 255, 255, 8, 5) != "+00042|0xff|0XFF|010|101" {
                return 2
            }
            if format("%-6s|%.3s|%c|%%", "hi", "abcdef", 65) != "hi    |abc|A|%" {
                return 3
            }
            if format("%.2f|%.1e|%g", 3.14159, 1250.0, 0.5) != "3.14|1.2e+03|0.5" {
                return 4
            }
            let hex_float = format("%a", 1.5)
            if not hex_float.starts_with("0x1.8") { return 5 }

            let quoted = format("%q|%q|%q|%q", "a\"b\nc", true, null, 7)
            if quoted != "\"a\\\"b\\nc\"|true|null|7" { return 6 }

            if "x=%d, y=%.2f".format(10, 3.5) != "x=10, y=3.50" { return 7 }

            let via_list = try_sprintf_list("%s=%d", ["k", 9]).unwrap()
            if via_list != "k=9" { return 8 }

            let e1 = try_sprintf("%z", 1) match {
                _! => false,
                error err => err == FormatError.InvalidSpec("invalid conversion specifier '%z'")
                    and err.to_string().contains("invalid format specifier"),
            }
            if not e1 { return 9 }

            let e2 = try_sprintf("%d %d", 1) match {
                _! => false,
                error FormatError.MissingArg(_) => true,
                error _ => false,
            }
            if not e2 { return 10 }

            let e3 = try_sprintf("%d", "nope") match {
                _! => false,
                error FormatError.TypeMismatch(_) => true,
                error _ => false,
            }
            if not e3 { return 11 }

            let e4 = try_sprintf("%c", -1) match {
                _! => false,
                error FormatError.OutOfRange(_) => true,
                error _ => false,
            }
            if not e4 { return 12 }

            printf("")
            eprintf("")
            42
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);

    let panic_src = r#"fn main() -> i64 { let _ = format("%d"); 0 }"#;
    let err = common::run_value(panic_src).unwrap_err();
    assert!(err.contains("missing format argument"), "{err}");
}

#[test]
fn pattern_find_captures_gmatch_and_gsub_variants() {
    let source = r#"
        use std.collections.*
        use std.pattern
        use std.pattern.{Match, PatternError}

        fn main() -> i64 {
            let m0? = pattern.find("hello 123 world", "%d+") else { return 1 }
            if m0.start != 6 or m0.end != 9 or m0.text != "123" or m0.captures.len() != 0 {
                return 2
            }
            let expected_m0 = Match { start: 6, end: 9, text: "123", captures: List() }
            if m0 != expected_m0 { return 3 }

            let plain_hit? = pattern.find("a+b*c?", "+b*", init = 1, plain = true) else { return 4 }
            if plain_hit.start != 1 or plain_hit.end != 4 or plain_hit.text != "+b*" { return 5 }

            if pattern.match_once("key = 42", "(%a+)%s*=%s*(%d+)") != "key" { return 6 }
            if pattern.match_once("key = 42", "%d+") != "42" { return 7 }
            if pattern.match_once("key = 42", "%d+", init = 7) != "2" { return 8 }
            if pattern.match_once("abc", "%d+") != null { return 9 }

            let caps0? = pattern.captures("pos:ab!", "()(%a+)()") else { return 10 }
            if caps0 != [0, "pos", 3] { return 11 }
            let caps? = pattern.captures("123:ab!", "()(%a+)()") else { return 10 }
            if caps != [4, "ab", 6] { return 11 }
            let no_group_caps? = pattern.captures("foo=99", "%d+") else { return 12 }
            if no_group_caps != ["99"] { return 13 }

            -- Balanced `%b()` and frontier `%f[%a]`
            if pattern.match_once("fn(a(b)c) + 1", "%b()") != "(a(b)c)" { return 14 }
            let words = List()
            for item in pattern.gmatch_iter("one two three", "%f[%a]%a+") {
                words.push(item.text)
            }
            if words != ["one", "two", "three"] { return 15 }

            -- Zero-length matches in `gmatch_iter` must never infinite-loop.
            let empty_spans = List()
            for item in pattern.gmatch_iter("aab", "a*") {
                empty_spans.push((item.start, item.end, item.text))
            }
            if empty_spans != [(0, 2, "aa"), (3, 3, "")] { return 16 }

            -- Anchored `^` in `gmatch_iter` must match at most once at index 0.
            let anchored = List()
            for item in pattern.gmatch_iter("abc", "^%a") {
                anchored.push(item.text)
            }
            if anchored != ["a"] { return 17 }

            -- `gsub`, `gsub_with`, `gsub_map`, and `escape`
            let (s1, n1) = pattern.gsub("hello world", "(%w+)", "%1-%1", limit = 1)
            if s1 != "hello-hello world" or n1 != 1 { return 18 }

            let (s2, n2) = pattern.gsub_with("aab", "a*", |m: Match| "[" ++ m.text ++ "]").unwrap()
            if s2 != "[aa]b[]" or n2 != 2 { return 19 }

            let (s3, n3) = pattern.gsub_with("10 20 30", "%d+", |m: Match| {
                if m.text == "20" { null } else { "<" ++ m.text ++ ">" }
            }).unwrap()
            if s3 != "<10> 20 <30>" or n3 != 3 { return 20 }

            let dict = Map()
            dict.set("name", "Nessa")
            dict.set("ver", 2)
            let (s4, n4) = pattern.gsub_map("$name v$ver ($missing)", "%$(%w+)", dict).unwrap()
            if s4 != "Nessa v2 ($missing)" or n4 != 3 { return 21 }

            let raw_magic = "^$()%.[]*+-?"
            let escaped = pattern.escape(raw_magic)
            let m_esc? = pattern.find("prefix " ++ raw_magic ++ " suffix", escaped) else {
                return 22
            }
            if m_esc.text != raw_magic { return 23 }

            let bad = pattern.try_find("abc", "[unclosed") match {
                _! => false,
                error PatternError.InvalidPattern(_) => true,
            }
            if not bad { return 24 }

            let bad_gmatch = pattern.gmatch("abc", "%") match {
                _! => false,
                error PatternError.InvalidPattern(_) => true,
            }
            if not bad_gmatch { return 25 }

            42
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn string_utf8_charpattern_and_radix_parsing() {
    let source = r#"
        use std.collections.*
        use std.pattern

        fn main() -> i64 {
            let uni = "aé中🚀"
            if uni.reverse() != "🚀中éa" { return 1 }
            if "".reverse() != "" { return 2 }
            if "ab".reverse_bytes() != [98, 97] { return 3 }

            if "ha".repeat_sep(0, "-") != "" { return 4 }
            if "ha".repeat_sep(-3, "-") != "" { return 5 }
            if "ha".repeat_sep(1, "-") != "ha" { return 6 }
            if "ha".repeat_sep(3, ", ") != "ha, ha, ha" { return 7 }

            if "abcdef".bytes_range(1, 4) != [98, 99, 100] { return 8 }
            if "abc".bytes_range(-5, 99) != [97, 98, 99] { return 9 }
            if "abc".bytes_range(2, 1) != List() { return 10 }

            -- "aé🚀" has UTF-8 byte offsets 0 ('a'), 1 ('é', 2 bytes), 3 ('🚀', 4 bytes), len = 7.
            let sample = "aé🚀"
            if sample.utf8_offset(1) != 0 { return 11 }
            if sample.utf8_offset(2) != 1 { return 12 }
            if sample.utf8_offset(3) != 3 { return 13 }
            if sample.utf8_offset(4) != 7 { return 14 }
            if sample.utf8_offset(5) != null { return 15 }
            if sample.utf8_offset(-1) != 3 { return 16 }
            if sample.utf8_offset(-2) != 1 { return 17 }
            if sample.utf8_offset(-3) != 0 { return 18 }
            if sample.utf8_offset(-4) != null { return 19 }
            if sample.utf8_offset(0, byte_pos = 5) != 3 { return 20 }
            if sample.utf8_offset(1, byte_pos = 2) != null { return 21 }

            if sample.codepoints() != [(0, 97), (1, 233), (3, 128640)] { return 22 }

            if from_codes([97, 233, 128640]) != "aé🚀" { return 23 }
            if from_char_codes(78, 101, 115, 115, 97) != "Nessa" { return 24 }
            if from_codes([55296]) != null { return 25 }
            if from_char_codes(-1) != null { return 26 }

            let scalars = List()
            for m in pattern.gmatch_iter(uni, UTF8_CHARPATTERN()) {
                scalars.push(m.text)
            }
            if scalars != ["a", "é", "中", "🚀"] or utf8_charpattern() != UTF8_CHARPATTERN() { return 27 }

            if "  -0x2a\n".parse_int_radix(0) != -42 { return 28 }
            if "0b101010".parse_int_radix(0) != 42 { return 29 }
            if "0o52".parse_int_radix(0) != 42 { return 30 }
            if "  zz ".parse_int_radix(36) != 1295 { return 31 }
            if "-9223372036854775808".parse_int_radix(10) != MIN_INTEGER() { return 32 }
            if "999999999999999999999".parse_int_radix(10) != null { return 33 }
            if "12g".parse_int_radix(10) != null { return 34 }

            42
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn bytes_pack_unpack_utf8_hex_and_base64() {
    let source = r#"
        use std.bytes
        use std.bytes.PackError
        use std.collections.*

        fn main() -> i64 {
            if bytes.pack_size("<bHI4").unwrap() != 7 { return 1 }
            if bytes.pack_size("!4 b I4").unwrap() != 8 { return 2 }

            let pkt = bytes.pack("<bHI4", -2, 513, 0x01020304).unwrap()
            if pkt != [254, 1, 2, 4, 3, 2, 1] { return 3 }
            let (vals, next_pos) = bytes.unpack("<bHI4", pkt).unwrap()
            if vals != [-2, 513, 0x01020304] or next_pos != 7 { return 4 }

            let be_pkt = bytes.pack(">i2 s2 z c3", -258, "hi", "ok", "xyz").unwrap()
            let (be_vals, end_pos) = bytes.unpack(">i2 s2 z c3", be_pkt).unwrap()
            if be_vals != [-258, "hi", "ok", "xyz"] or end_pos != be_pkt.len() { return 5 }

            -- Chained unpacking via `.pos`
            let two = bytes.pack("<I2I2", 100, 200).unwrap()
            let (v1, p1) = bytes.unpack("<I2", two, pos = 0).unwrap()
            let (v2, p2) = bytes.unpack("<I2", two, pos = p1).unwrap()
            if v1 != [100] or v2 != [200] or p2 != 4 { return 6 }

            -- PackError variants
            let err_fmt = bytes.pack_size("z") match {
                _! => false,
                error PackError.InvalidFormat(_) => true,
                error _ => false,
            }
            if not err_fmt { return 7 }

            let err_short = bytes.unpack("<I4", [1, 2]) match {
                _! => false,
                error PackError.DataTooShort(_) => true,
                error _ => false,
            }
            if not err_short { return 8 }

            let err_range = bytes.pack("B", 300) match {
                _! => false,
                error PackError.ValueOutOfRange(_) => true,
                error _ => false,
            }
            if not err_range { return 9 }

            let err_missing = bytes.pack("<I2I2", 1) match {
                _! => false,
                error PackError.PackFailed(_) => true,
                error _ => false,
            }
            if not err_missing { return 10 }

            -- UTF-8 helpers
            let utf8_raw = bytes.from_string("hé!")
            if utf8_raw != [104, 195, 169, 33] { return 11 }
            if bytes.to_utf8(utf8_raw) != "hé!" { return 12 }
            if bytes.to_utf8([255, 65]) != null { return 13 }
            if bytes.to_utf8_lossy([255, 65]) != "\uFFFD" ++ "A" { return 14 }

            -- Hex roundtrip and error cases
            if bytes.to_hex([0, 15, 16, 255]) != "000f10ff" { return 15 }
            let hex_ok? = bytes.from_hex("000F10ff") else { return 16 }
            if hex_ok != [0, 15, 16, 255] { return 16 }
            let hex_empty? = bytes.from_hex("") else { return 17 }
            if hex_empty != List() { return 17 }
            if bytes.from_hex("abc") != null { return 18 }
            if bytes.from_hex("0g") != null { return 19 }

            -- RFC 4648 Base64 vectors and strict validation
            if bytes.base64_encode(List()) != "" { return 20 }
            if bytes.base64_encode(bytes.from_string("f")) != "Zg==" { return 21 }
            if bytes.base64_encode(bytes.from_string("fo")) != "Zm8=" { return 22 }
            if bytes.base64_encode(bytes.from_string("foo")) != "Zm9v" { return 23 }
            if bytes.base64_encode(bytes.from_string("foob")) != "Zm9vYg==" { return 24 }
            if bytes.base64_encode(bytes.from_string("fooba")) != "Zm9vYmE=" { return 25 }
            if bytes.base64_encode(bytes.from_string("foobar")) != "Zm9vYmFy" { return 26 }

            let bin = [0, 255, 128, 63, 251, 239]
            let b64 = bytes.base64_encode(bin)
            if b64 != "AP+AP/vv" { return 27 }
            let dec_bin? = bytes.base64_decode(b64) else { return 28 }
            if dec_bin != bin { return 28 }
            let dec_f? = bytes.base64_decode("Zg==") else { return 29 }
            if dec_f != bytes.from_string("f") { return 29 }
            let dec_fo? = bytes.base64_decode("Zm8=") else { return 30 }
            if dec_fo != bytes.from_string("fo") { return 30 }
            let dec_foobar? = bytes.base64_decode("Zm9vYmFy") else { return 31 }
            if dec_foobar != bytes.from_string("foobar") { return 31 }
            let dec_empty? = bytes.base64_decode("") else { return 32 }
            if dec_empty != List() { return 32 }

            -- Reject malformed Base64
            if bytes.base64_decode("abc") != null { return 33 }
            if bytes.base64_decode("Zg==Zm9v") != null { return 34 }
            if bytes.base64_decode("Zh==") != null { return 35 }
            if bytes.base64_decode("Zm9=") != null { return 36 }
            if bytes.base64_decode("Zm$=") != null { return 37 }

            42
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn math_lua_parity_and_float_formatting() {
    let source = r#"
        fn main() -> i64 {
            if (not HUGE().is_infinite()) or (not HUGE().is_sign_positive()) or huge() != HUGE() { return 1 }
            if MAX_INTEGER() != 9223372036854775807 or max_integer() != MAX_INTEGER() { return 2 }
            if MIN_INTEGER() != -9223372036854775807 - 1 or min_integer() != MIN_INTEGER() { return 3 }

            if fmod(5.5, 2.0) != 1.5 or fmod(-5.5, 2.0) != -1.5 { return 4 }

            let (i1, f1) = modf(3.25)
            if i1 != 3.0 or f1 != 0.25 { return 5 }
            let (i2, f2) = modf(-3.25)
            if i2 != -3.0 or f2 != -0.25 { return 6 }
            let (i3, f3) = modf(-3.0)
            if i3 != -3.0 or f3 != 0.0 or (not f3.is_sign_negative()) { return 7 }
            let (i_inf, f_inf) = modf(-HUGE())
            if (not i_inf.is_infinite()) or (not i_inf.is_sign_negative()) or f_inf != 0.0 or (not f_inf.is_sign_negative()) {
                return 8
            }
            let (i_nan, f_nan) = modf(nan())
            if (not i_nan.is_nan()) or (not f_nan.is_nan()) { return 9 }

            let (mant, exp_val) = frexp(12.0)
            if mant != 0.75 or exp_val != 4 { return 10 }
            if ldexp(mant, exp_val) != 12.0 { return 11 }
            let sub = min_positive() / 2.0
            let (sub_m, sub_e) = sub.frexp()
            if sub_m.ldexp(sub_e) != sub { return 12 }

            if (3.14159).to_fixed(2) != "3.14" { return 13 }
            if (1500.0).to_exp(digits = 2) != "1.50e+3" { return 14 }
            if (1500.0).to_exp() != "1.5e+3" { return 15 }
            if (0.0012345).to_precision(3) != "0.00123" { return 16 }

            if (not ult(1, 2)) or (not ult(1, -1)) or ult(-1, 1) or (not ult(-2, -1)) { return 17 }

            if math_type(42) != "integer"
                or math_type((7).as(i8)) != "integer"
                or math_type((7).as(u32)) != "integer"
                or math_type((7).as(u128)) != "integer" {
                return 18
            }
            if math_type(3.5) != "float" or math_type((3.5).as(f32)) != "float" {
                return 19
            }
            if math_type("42") != null or math_type(true) != null or math_type(null) != null {
                return 20
            }

            if to_exact_i64(42) != 42 { return 21 }
            if to_exact_i64((12).as(i16)) != 12 { return 22 }
            if to_exact_i64((200).as(u8)) != 200 { return 23 }
            if to_exact_i64(u64_max()) != null { return 24 }
            if to_exact_i64((-42.0)) != -42 { return 25 }
            if to_exact_i64(3.25) != null or to_exact_i64(HUGE()) != null or to_exact_i64(nan()) != null {
                return 26
            }
            if to_exact_i64("  -0x2a ") != -42 or to_exact_i64("3.5") != null {
                return 27
            }

            42
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

fn rust_xoshiro256ss_reference(s1: i64, s2: i64) -> [u64; 4] {
    fn sm64(state: u64) -> (u64, u64) {
        let next = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = next;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (next, z)
    }
    let (st1, mut s0) = sm64(s1 as u64);
    let (st2, mut w1) = sm64(st1 ^ (s2 as u64));
    let (st3, mut w2) = sm64(st2);
    let (_, mut w3) = sm64(st3);
    if s0 == 0 && w1 == 0 && w2 == 0 && w3 == 0 {
        s0 = 1;
    }
    let mut out = [0u64; 4];
    for slot in &mut out {
        let res = w1.wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = w1 << 17;
        w2 ^= s0;
        w3 ^= w1;
        w1 ^= w2;
        s0 ^= w3;
        w2 ^= t;
        w3 = w3.rotate_left(45);
        *slot = res;
    }
    out
}

#[test]
fn rand_vm_prng_and_pure_nessa_xoshiro256ss_match_reference() {
    let expected = rust_xoshiro256ss_reference(123456789, -987654321);
    let source = format!(
        r#"
        use std.collections.*
        use std.rand
        use std.rand.Rng

        fn main() -> i64 {{
            let _ = rand.seed()
            let used1 = rand.seed(s1 = 777, s2 = 888)
            let a1 = rand.next_u64()
            let a2 = rand.next_u64()
            let f = rand.random()
            if f < 0.0 or f >= 1.0 {{ return 1 }}

            let used2 = rand.seed(s1 = 777, s2 = 888)
            if used1 != used2 {{ return 2 }}
            if rand.next_u64() != a1 or rand.next_u64() != a2 {{ return 3 }}

            var idx = 0
            while idx < 50 {{
                let v = rand.random_int(-5, 5)
                if v < -5 or v > 5 {{ return 4 }}
                let u = rand.random_up_to(7)
                if u < 1 or u > 7 {{ return 5 }}
                idx += 1
            }}
            let _full = rand.random_up_to(0)
            let _span = rand.random_int(MIN_INTEGER(), MAX_INTEGER())

            let items = [10, 20, 30, 40, 50]
            rand.shuffle(items)
            if items.len() != 5 or (not items.contains(10)) or (not items.contains(50)) {{
                return 6
            }}
            if rand.choice(List()) != null {{ return 7 }}
            if not items.contains(rand.choice(items)) {{ return 8 }}

            -- Pure-Nessa Xoshiro256** exact bit-level reference check
            let rng = Rng.from_seed(123456789, s2 = -987654321)
            let r0: u64 = {}
            let r1: u64 = {}
            let r2: u64 = {}
            let r3: u64 = {}
            if rng.next_u64() != r0 {{ return 9 }}
            if rng.next_u64() != r1 {{ return 10 }}
            if rng.next_u64() != r2 {{ return 11 }}
            if rng.next_u64() != r3 {{ return 12 }}

            let rf = rng.next_f64()
            if rf < 0.0 or rf >= 1.0 {{ return 13 }}
            if rng.range_int(99, 99) != 99 {{ return 14 }}
            let _full_rng = rng.range_int(MIN_INTEGER(), MAX_INTEGER())

            let deck = ["a", "b", "c", "d"]
            rng.shuffle(deck)
            if deck.len() != 4 or (not deck.contains("a")) or (not deck.contains("d")) {{
                return 15
            }}
            if rng.choice(List()) != null or (not deck.contains(rng.choice(deck))) {{
                return 16
            }}

            42
        }}
        "#,
        expected[0], expected[1], expected[2], expected[3]
    );
    assert_eq!(common::run_value(&source).unwrap(), 42);

    let bad_range = r#"
        use std.rand
        fn main() -> i64 { rand.random_int(10, 5) }
    "#;
    assert!(common::run_value(bad_range).is_err());
}

#[test]
fn scalar_small_width_integers_and_f32_methods() {
    let source = r#"
        fn main() -> i64 {
            if i8.bits() != 8 or i8.min_value() != (-128).as(i8) or i8.max_value() != (127).as(i8) {
                return 1
            }
            let z8 = (0).as(i8)
            let n8 = (-42).as(i8)
            let min8 = i8.min_value()
            if (not z8.is_zero()) or z8.is_positive() or z8.is_negative() or z8.signum() != (0).as(i8) {
                return 2
            }
            if (not n8.is_negative()) or n8.signum() != (-1).as(i8) or n8.checked_abs() != (42).as(i8) {
                return 3
            }
            if min8.checked_abs() != null
                or min8.wrapping_abs() != min8
                or min8.saturating_abs() != i8.max_value() {
                return 4
            }

            if i16.bits() != 16 or i16.min_value() != (-32768).as(i16) or i16.max_value() != (32767).as(i16) {
                return 5
            }
            let min16 = i16.min_value()
            if min16.checked_abs() != null
                or min16.wrapping_abs() != min16
                or min16.saturating_abs() != i16.max_value()
                or ((15).as(i16)).signum() != (1).as(i16) {
                return 6
            }

            if i32.bits() != 32 or i32.min_value() != (-2147483648).as(i32) or i32.max_value() != (2147483647).as(i32) {
                return 7
            }
            let min32 = i32.min_value()
            if min32.checked_abs() != null
                or min32.wrapping_abs() != min32
                or min32.saturating_abs() != i32.max_value()
                or ((-9).as(i32)).checked_abs() != (9).as(i32) {
                return 8
            }

            if u8.bits() != 8 or u8.min_value() != (0).as(u8) or u8.max_value() != (255).as(u8) {
                return 9
            }
            let b_ascii = (65).as(u8)
            let b_latin = (233).as(u8)
            if (not u8.min_value().is_zero()) or b_ascii.is_zero() { return 10 }
            if (not b_ascii.is_ascii()) or b_latin.is_ascii() { return 11 }
            if b_ascii.to_char() != 'A' or b_latin.to_char() != 'é' { return 12 }

            if u16.bits() != 16 or u16.min_value() != (0).as(u16) or u16.max_value() != (65535).as(u16) or (not u16.min_value().is_zero()) {
                return 13
            }
            if u32.bits() != 32 or u32.min_value() != (0).as(u32) or u32.max_value() != (4294967295).as(u32) or (not u32.min_value().is_zero()) {
                return 14
            }

            -- f32 classification and rounding/math delegation
            let f_val = (-3.75).as(f32)
            if f_val.abs() != (3.75).as(f32) { return 15 }
            if f_val.floor() != (-4.0).as(f32) or f_val.ceil() != (-3.0).as(f32) { return 16 }
            if f_val.round() != (-4.0).as(f32) or f_val.trunc() != (-3.0).as(f32) { return 17 }
            if f_val.fract() != (-0.75).as(f32) { return 18 }
            if ((9.0).as(f32)).sqrt() != (3.0).as(f32) { return 19 }

            let f_nan = nan().as(f32)
            let f_inf = infinity().as(f32)
            let f_neg_zero = (-0.0).as(f32)
            -- 2^-130 is a subnormal IEEE-754 binary32 value!
            let f_sub = ldexp(1.0, -130).as(f32)
            if (not f_nan.is_nan()) or f_nan.is_finite() { return 20 }
            if (not f_inf.is_infinite()) or f_inf.is_finite() { return 21 }
            if (not f_neg_zero.is_zero()) or (not f_neg_zero.is_sign_negative()) or f_neg_zero.is_normal() or f_neg_zero.is_subnormal() {
                return 22
            }
            if (not f_sub.is_subnormal()) or f_sub.is_normal() or f_sub.is_zero() {
                return 23
            }
            if (not ((1.0).as(f32)).is_normal()) or ((1.0).as(f32)).is_subnormal() or (not ((1.0).as(f32)).is_sign_positive()) {
                return 24
            }

            42
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn utf8_charpattern_hex_escapes_utf8_helpers_pointer_format_and_xs_alignment() {
    let source = r#"
        use std.pattern
        use std.bytes
        use std.fmt

        fn collect_texts(text: String, pat: String) -> List {
            let it! = pattern.gmatch(text, pat) else {
                return List()
            }
            let out = List()
            for m in it {
                out.push(m.text)
            }
            out
        }

        fn main() -> i64 {
            -- 1. UTF8_CHARPATTERN must keep U+0080..=U+00BF scalars ('©', '®') and '\n' separate!
            let cp = utf8_charpattern()
            if cp != pattern.UTF8_CHARPATTERN or cp != UTF8_CHARPATTERN() {
                return 1
            }
            if collect_texts("a©b", cp) != ["a", "©", "b"] {
                return 2
            }
            if collect_texts("©®", cp) != ["©", "®"] {
                return 3
            }
            if collect_texts("a\nb", cp) != ["a", "\n", "b"] {
                return 4
            }
            if collect_texts("中😀!", cp) != ["中", "😀", "!"] {
                return 5
            }
            -- Standalone %BHH escape matches exact UTF-8 lead/continuation bytes of '©' (0xC2 0xA9)
            if pattern.match_once("x©y", "%BC2%BA9") != "©" {
                return 6
            }
            let bad_hex = pattern.try_find("abc", "%BZZ")
            if not (bad_hex matches error pattern.PatternError.InvalidPattern(_)) {
                return 7
            }

            -- 2. String.utf8_offset with explicit byte_pos = -1 vs omitted byte_pos = null
            if "abc".utf8_offset(0, byte_pos = -1) != 2 {
                return 8
            }
            if "abc".utf8_offset(1, byte_pos = -1) != 2 {
                return 9
            }
            if "abc".utf8_offset(-1, byte_pos = -1) != 1 {
                return 10
            }
            if "a©b".utf8_offset(0, byte_pos = -2) != 1 {
                return 11
            }
            if "a©b".utf8_offset(-1) != 3 or "a©b".utf8_offset(2) != 1 {
                return 12
            }

            -- 3. String.sub, byte_slice, from_chars, utf8_codes, utf8_codepoint, strict utf8_len
            if "a©b🎉z".sub(2, end = 4) != "©b🎉" {
                return 13
            }
            if "abcdef".sub(-3) != "def" or "abcdef".sub(0, end = 0) != "" {
                return 14
            }
            if "a©".byte_slice(2, end = 3) != [0xC2, 0xA9] or "abc".byte_slice(-1) != [99] {
                return 15
            }
            if from_chars(['N', '©', '🎉']) != "N©🎉" {
                return 16
            }
            let codes = utf8_codes("a©")
            let mcodes = "a©".utf8_codes()
            if codes.len() != 2
                or mcodes.len() != 2
                or codes.get(0).as((i64, i64)) != (0, 97)
                or codes.get(1).as((i64, i64)) != (1, 169)
                or mcodes.get(1).as((i64, i64)) != (1, 169) {
                return 17
            }
            let cp0? = "a©b".utf8_codepoint() else { return 18 }
            let cp1? = "a©b".utf8_codepoint(start = 1) else { return 18 }
            let cp_all? = "a©b".utf8_codepoint(start = 0, end = -1) else { return 18 }
            if cp0 != [97]
                or cp1 != [169]
                or "a©b".utf8_codepoint(start = 2) != null
                or cp_all != [97, 169, 98] {
                return 18
            }

            let (ok_cnt, ok_err) = utf8_len("a©🎉")
            if ok_cnt != 3 or ok_err != null {
                return 19
            }
            -- Mid-character byte start on valid String fails at continuation byte offset 2
            let (mid_cnt, mid_err) = "a©b".utf8_len(start = 2)
            if mid_cnt != null or mid_err != 2 {
                return 20
            }
            -- Overlong 2-byte NUL (0xC0 0x80), 3-byte overlong '/' (0xE0 0x80 0xAF),
            -- UTF-16 surrogate U+D800 (0xED 0xA0 0x80), > U+10FFFF (0xF4 0x90 0x80 0x80),
            -- and truncated 2-byte sequence ([0x61, 0xC2]) must all fail at exact byte offset!
            let (c1, e1) = utf8_len([0x61, 0xC0, 0x80])
            let (c2, e2) = utf8_len([0xE0, 0x80, 0xAF])
            let (c3, e3) = utf8_len([0x61, 0x62, 0xED, 0xA0, 0x80])
            let (c4, e4) = utf8_len([0xF4, 0x90, 0x80, 0x80])
            let (c5, e5) = utf8_len([0x61, 0xC2])
            if c1 != null or e1 != 1 { return 21 }
            if c2 != null or e2 != 0 { return 22 }
            if c3 != null or e3 != 2 { return 23 }
            if c4 != null or e4 != 0 { return 24 }
            if c5 != null or e5 != 1 { return 25 }

            -- 4. %p pointer formatting: heap objects format as 0x..., immediates & numbers as (null)
            let lst = [1, 2]
            let p_lst = format("%p", lst)
            if (not p_lst.starts_with("0x")) or p_lst.len() < 3 {
                return 26
            }
            let p_pad = format("%-24p", lst)
            if p_pad.len() != 24 or (not p_pad.starts_with(p_lst)) {
                return 27
            }
            let big_i128: i128 = i128_max()
            if format("%p|%p|%p|%p|%p", null, 42, 3.5, true, big_i128)
                != "(null)|(null)|(null)|(null)|(null)" {
                return 28
            }
            let bad_p = fmt.try_sprintf("%.4p", lst)
            if not (bad_p matches error fmt.FormatError.InvalidSpec(_)) {
                return 29
            }

            -- 5. bytes.pack / unpack / pack_size with Xs and Xs2 / Xs4 alignment
            let sz_unaligned! = bytes.pack_size("<bXs4b") else { return 30 }
            let sz_aligned4! = bytes.pack_size("<!4bXs4b") else { return 30 }
            let sz_aligned8! = bytes.pack_size("<!8bXsb") else { return 30 }
            if sz_unaligned != 2 or sz_aligned4 != 5 or sz_aligned8 != 9 {
                return 31
            }
            let packed! = bytes.pack("<!4bXs2s2", 7, "hi") else { return 32 }
            if packed != [7, 0, 2, 0, 104, 105] {
                return 33
            }
            let (vals, next_pos)! = bytes.unpack("<!4bXs2s2", packed) else { return 34 }
            if next_pos != 6 or vals != [7, "hi"] {
                return 35
            }

            42
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}
