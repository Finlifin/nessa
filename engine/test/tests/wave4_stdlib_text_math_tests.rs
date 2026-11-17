//! Comprehensive integration tests for Wave 4 standard library modules:
//! `std.fmt`, `std.pattern`, `std.string`, `std.conv`, `std.bytes`, `std.math`, `std.rand`, and `std.scalar`.

mod common;

#[test]
fn fmt_specifiers_prelude_exports_and_typed_errors() {
    let source = r#"
        use std.fmt
        use std.fmt.FormatError

        fn run_checks() -> !FormatError i64 {
            if format("%d %i %u", -42, 42, 99)! != "-42 42 99" { return 1 }
            if format("%+06d|%#x|%#X|%#o|%b", 42, 255, 255, 8, 5)! != "+00042|0xff|0XFF|010|101" {
                return 2
            }
            if format("%-6s|%.3s|%c|%%", "hi", "abcdef", 65)! != "hi    |abc|A|%" {
                return 3
            }
            if format("%.2f|%.1e|%g", 3.14159, 1250.0, 0.5)! != "3.14|1.2e+03|0.5" {
                return 4
            }
            let hex_float = format("%a", 1.5)!
            if not hex_float.starts_with("0x1.8") { return 5 }

            let quoted = format("%q|%q|%q|%q", "a\"b\nc", true, null, 7)!
            if quoted != "\"a\\\"b\\nc\"|true|null|7" { return 6 }

            if "x=%d, y=%.2f".format(10, 3.5) != "x=10, y=3.50" { return 7 }

            let via_list = fmt.format_list("%s=%d", ["k", 9])!
            if via_list != "k=9"
                or fmt.sprintf("%s=%d", "k", 9) != "k=9"
                or fmt.sprintf_list("%s=%d", ["k", 9]) != "k=9"
                or fmt.must_format("%s=%d", "k", 9) != "k=9" {
                return 8
            }

            let e1 = format("%z", 1) match {
                _! => false,
                error err => err == FormatError.InvalidSpec("invalid conversion specifier '%z'")
                    and err.to_string().contains("invalid format specifier"),
            }
            if not e1 { return 9 }

            let e2 = format("%d %d", 1) match {
                _! => false,
                error FormatError.MissingArg(_) => true,
                error _ => false,
            }
            if not e2 { return 10 }

            let e3 = format("%d", "nope") match {
                _! => false,
                error FormatError.TypeMismatch(_) => true,
                error _ => false,
            }
            if not e3 { return 11 }

            let e4 = format("%c", -1) match {
                _! => false,
                error FormatError.OutOfRange(_) => true,
                error _ => false,
            }
            if not e4 { return 12 }

            printf("")
            eprintf("")
            42
        }

        fn main() -> i64 {
            run_checks() ! { _ => -999 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);

    let panic_src = r#"fn main() -> i64 { let _ = "%d".format(); 0 }"#;
    let err = common::run_value(panic_src).unwrap_err();
    assert!(err.contains("missing format argument"), "{err}");
}

#[test]
fn pattern_find_captures_gmatch_and_gsub_variants() {
    let source = r#"
        use std.collections.*
        use std.pattern
        use std.pattern.{PatternError, PatternMatch}

        enum ReplError {
            ForbiddenToken(token: String),
        }

        fn run_checks() -> !PatternError i64 {
            let m0_opt = pattern.find("hello 123 world", "%d+")!
            let m0? = m0_opt else { return 1 }
            if m0.start != (6).as(usize)
                or m0.end != (9).as(usize)
                or m0.len() != (3).as(usize)
                or m0.is_empty()
                or m0.slice_of("hello 123 world") != "123"
                or m0.captures.len() != 0
                or m0.to_string() != "PatternMatch(6..9)" {
                return 2
            }
            let expected_m0 = PatternMatch { start: 6, end: 9, captures: List() }
            if m0 != expected_m0 { return 3 }

            let plain_opt = pattern.find("a+b*c?", "+b*", start = 1, plain = true)!
            let plain_hit? = plain_opt else { return 4 }
            if plain_hit.start != (1).as(usize)
                or plain_hit.end != (4).as(usize)
                or plain_hit.slice_of("a+b*c?") != "+b*" {
                return 5
            }

            let kv_opt = pattern.find("key = 42", "(%a+)%s*=%s*(%d+)")!
            let kv? = kv_opt else { return 6 }
            if kv.captures != ["key", "42"] { return 6 }
            if (not pattern.is_match("key = 42", "%d+")!) or (pattern.is_match("abc", "%d+")!) {
                return 7
            }
            let tail_opt = pattern.find("key = 42", "%d+", start = 7)!
            let tail_hit? = tail_opt else { return 8 }
            if tail_hit.slice_of("key = 42") != "2" { return 8 }
            if pattern.find("abc", "%d+")! != null { return 9 }

            let caps0_opt = pattern.find("pos:ab!", "()(%a+)()")!
            let caps0? = caps0_opt else { return 10 }
            if caps0.captures != [0, "pos", 3] { return 11 }
            let caps_opt = pattern.find("123:ab!", "()(%a+)()")!
            let caps? = caps_opt else { return 10 }
            if caps.captures != [4, "ab", 6] { return 11 }
            let no_group_opt = pattern.find("foo=99", "%d+")!
            let no_group? = no_group_opt else { return 12 }
            if no_group.captures != List() or no_group.slice_of("foo=99") != "99" { return 13 }

            -- Balanced `%b()` and frontier `%f[%a]` via `find`, `find_iter`, and `find_all`
            let bal_opt = pattern.find("fn(a(b)c) + 1", "%b()")!
            let bal? = bal_opt else { return 14 }
            if bal.slice_of("fn(a(b)c) + 1") != "(a(b)c)" { return 14 }

            let phrase = "one two three"
            let words = List()
            for item in (pattern.find_iter(phrase, "%f[%a]%a+")!) {
                words.push(item.slice_of(phrase))
            }
            let all_matches = pattern.find_all(phrase, "%f[%a]%a+")!
            if words != ["one", "two", "three"] or all_matches.len() != 3 { return 15 }

            -- Zero-length matches in `find_iter` must never infinite-loop.
            let empty_spans = List()
            for item in (pattern.find_iter("aab", "a*")!) {
                empty_spans.push((item.start.as(i64), item.end.as(i64), item.slice_of("aab"), item.is_empty()))
            }
            if empty_spans != [(0, 2, "aa", false), (3, 3, "", true)] { return 16 }

            -- Anchored `^` in `find_iter` must match at most once at index 0.
            let anchored = List()
            for item in (pattern.find_iter("abc", "^%a")!) {
                anchored.push(item.slice_of("abc"))
            }
            if anchored != ["a"] { return 17 }

            -- `replace`, `replace_with` (including `limit`, map lookup, and `!Error` propagation), and `escape`
            let (s1, n1) = pattern.replace("hello world", "(%w+)", "%1-%1", limit = 1)!
            if s1 != "hello-hello world" or n1 != (1).as(usize) { return 18 }

            let (s_lim0, n_lim0) = pattern.replace("hello world", "%w+", "X", limit = 0)!
            if s_lim0 != "hello world" or n_lim0 != (0).as(usize) { return 18 }

            let (s2, n2) = pattern.replace_with("aab", "a*", |m: PatternMatch| "[" ++ m.slice_of("aab") ++ "]")!
            if s2 != "[aa]b[]" or n2 != (2).as(usize) { return 19 }

            let src3 = "10 20 30"
            let (s3, n3) = pattern.replace_with(src3, "%d+", |m: PatternMatch| {
                let tok = m.slice_of(src3)
                if tok == "20" { tok } else { "<" ++ tok ++ ">" }
            })!
            if s3 != "<10> 20 <30>" or n3 != (3).as(usize) { return 20 }

            let dict = Map()
            dict.set("name", "Nessa")
            dict.set("ver", "2")
            let tpl = "$name v$ver ($missing)"
            let (s4, n4) = pattern.replace_with(tpl, "%$(%w+)", |m: PatternMatch| {
                let key = m.captures.get(0).as(String)
                if dict.contains(key) { dict.get(key).as(String) } else { m.slice_of(tpl) }
            })!
            if s4 != "Nessa v2 ($missing)" or n4 != (3).as(usize) { return 21 }

            let fallible_repl = pattern.replace_with("ok bad tail", "%a+", |m: PatternMatch| -> !ReplError String {
                let word = m.slice_of("ok bad tail")
                if word == "bad" {
                    return error ReplError.ForbiddenToken(word)
                }
                word.to_uppercase()
            }) match {
                _! => false,
                error ReplError.ForbiddenToken(tok) => tok == "bad",
                error _ => false,
            }
            if not fallible_repl { return 22 }

            let raw_magic = "^$()%.[]*+-?"
            let escaped = pattern.escape(raw_magic)
            let esc_opt = pattern.find("prefix " ++ raw_magic ++ " suffix", escaped)!
            let m_esc? = esc_opt else { return 23 }
            if m_esc.slice_of("prefix " ++ raw_magic ++ " suffix") != raw_magic { return 23 }

            let bad = pattern.find("abc", "[unclosed") match {
                _! => false,
                error err => err match {
                    PatternError.InvalidPattern(_) => err.to_string().contains("invalid pattern"),
                },
            }
            if not bad { return 24 }

            let bad_iter = pattern.find_iter("abc", "%") match {
                _! => false,
                error PatternError.InvalidPattern(_) => true,
            }
            if not bad_iter { return 25 }

            42
        }

        fn main() -> i64 {
            run_checks() ! { _ => -999 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn string_utf8_charpattern_and_radix_parsing() {
    let source = r#"
        use std.collections.*
        use std.conv

        fn run_checks() -> !ParseError i64 {
            let uni = "aé中🚀"
            if uni.reverse() != "🚀中éa" { return 1 }
            if "".reverse() != "" { return 2 }
            if "ab".reverse_bytes() != [98, 97] { return 3 }

            -- `String.repeat` with optional `.sep`
            if "ha".repeat(0, sep = "-") != "" { return 4 }
            if "ha".repeat(1, sep = "-") != "ha" { return 5 }
            if "ha".repeat(3) != "hahaha" { return 6 }
            if "ha".repeat(3, sep = ", ") != "ha, ha, ha" { return 7 }

            -- `to_bytes`, `bytes_iter`, `chars_iter`, and `char_indices_iter`
            if "abcdef".to_bytes().slice(1, 4).to_list() != [98, 99, 100] { return 8 }
            if "ab".bytes_iter().collect() != [(97).as(u8), (98).as(u8)] { return 9 }
            if uni.chars_iter().collect() != ['a', 'é', '中', '🚀'] { return 10 }

            -- "aé🚀" has UTF-8 byte offsets 0 ('a'), 1 ('é', 2 bytes), 3 ('🚀', 4 bytes), len = 7.
            let sample = "aé🚀"
            if sample.len() != 7 or sample.char_len() != 3 or sample.char_count() != (3).as(usize) {
                return 11
            }
            if (not sample.is_char_boundary(0))
                or (not sample.is_char_boundary(1))
                or sample.is_char_boundary(2)
                or (not sample.is_char_boundary(3))
                or sample.is_char_boundary(5)
                or (not sample.is_char_boundary(7)) {
                return 12
            }
            if sample.floor_char_boundary(5) != 3 or sample.ceil_char_boundary(4) != 7 {
                return 13
            }
            if sample.char_at_byte(0) != 'a'
                or sample.char_at_byte(1) != 'é'
                or sample.char_at_byte(2) != null
                or sample.char_at_byte(3) != '🚀'
                or sample.char_at_byte(7) != null {
                return 14
            }

            let idx_codes = List()
            for (offset, ch) in sample.char_indices_iter() {
                idx_codes.push((offset.as(i64), ch.to_code()))
            }
            if idx_codes != [(0, 97), (1, 233), (3, 128640)] { return 15 }

            if sample.char_slice(1, end = 3) != "é🚀" or sample.char_slice(2) != "🚀" {
                return 16
            }
            if sample.byte_slice(1, end = 3) != "é"
                or sample.byte_slice(1, end = 2) != null
                or sample.byte_slice(3) != "🚀" {
                return 17
            }

            if from_codes([97, 233, 128640]) != "aé🚀" { return 18 }
            if String.from_chars(['N', 'e', 's', 's', 'a']) != "Nessa" { return 19 }
            if from_codes([55296]) != null or from_codes([-1]) != null { return 20 }

            -- First-class `Type` parsing (`String.parse`, `String.try_parse`, `conv.parse`, `conv.try_parse`)
            let p_hex: i64 = "-2a".parse(i64, radix = 16)!
            let p_bin: i64 = "101010".parse(i64, radix = 2)!
            let p_oct: u16 = conv.parse(u16, "52", radix = 8)!
            let p_b36: i32 = "zz".parse(i32, radix = 36)!
            let p_min: i64 = "-9223372036854775808".parse(i64)!
            let p_u64: u64 = "18446744073709551615".parse(u64)!
            let p_f64: f64 = "-12.5e2".parse(f64)!
            let p_bool: bool = "false".parse(bool)!
            let p_char: char = "🚀".parse(char)!
            if p_hex != -42
                or p_bin != 42
                or p_oct != (42).as(u16)
                or p_b36 != (1295).as(i32)
                or p_min != i64_min()
                or p_u64 != u64_max()
                or p_f64 != -1250.0
                or p_bool
                or p_char != '🚀' {
                return 21
            }

            if "999999999999999999999".try_parse(i64) != null
                or "12g".try_parse(i64) != null
                or "  42".try_parse(i64) != null
                or conv.try_parse(u8, "256") != null
                or conv.try_parse(u8, "ff", radix = 16) != (255).as(u8) {
                return 22
            }

            -- Exhaustive `ParseError` variant checks
            let err_empty = "".parse(i64) match {
                _! => false,
                error ParseError.Empty => true,
                error _ => false,
            }
            let err_digit = "12g".parse(i64, radix = 16) match {
                _! => false,
                error ParseError.InvalidDigit(ch, r) => ch == 'g' and r == 16,
                error _ => false,
            }
            let err_fmt = "+".parse(i64) match {
                _! => false,
                error ParseError.InvalidFormat(s) => s == "+",
                error _ => false,
            }
            let err_radix = "10".parse(i64, radix = 37) match {
                _! => false,
                error ParseError.InvalidRadix(r) => r == 37,
                error _ => false,
            }
            let err_over = "128".parse(i8) match {
                _! => false,
                error err => err == ParseError.Overflow("i8")
                    and err.to_string().contains("out of range for i8"),
            }
            let dyn_ty: Type = String
            let err_unsup = conv.parse(dyn_ty, "abc") match {
                _! => false,
                error ParseError.UnsupportedType(_) => true,
                error _ => false,
            }
            if (not err_empty)
                or (not err_digit)
                or (not err_fmt)
                or (not err_radix)
                or (not err_over)
                or (not err_unsup) {
                return 23
            }

            42
        }

        fn main() -> i64 {
            run_checks() ! { _ => -999 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn bytes_pack_unpack_utf8_hex_and_base64() {
    let source = r#"
        use std.bytes
        use std.bytes.{DecodeError, PackError}
        use std.collections.*

        fn run_checks() -> ![PackError, DecodeError, Utf8Error] i64 {
            if bytes.pack_size("<bHI4")! != 7 { return 1 }
            if bytes.pack_size("!4 b I4")! != 8 { return 2 }

            let pkt = bytes.pack("<bHI4", -2, 513, 0x01020304)!
            if pkt != Bytes.from_list([254, 1, 2, 4, 3, 2, 1]) or pkt.to_list() != [254, 1, 2, 4, 3, 2, 1] {
                return 3
            }
            let (vals, next_pos) = pkt.unpack("<bHI4")!
            if vals != [-2, 513, 0x01020304] or next_pos != (7).as(usize) { return 4 }

            let be_pkt = Bytes.pack(">i2 s2 z c3", -258, "hi", "ok", "xyz")!
            let (be_vals, end_pos) = bytes.unpack(">i2 s2 z c3", be_pkt)!
            if be_vals != [-258, "hi", "ok", "xyz"] or end_pos != be_pkt.len() { return 5 }

            -- Chained unpacking via `.pos`
            let two = bytes.pack("<I2I2", 100, 200)!
            let (v1, p1) = two.unpack("<I2", pos = 0)!
            let (v2, p2) = two.unpack("<I2", pos = p1)!
            if v1 != [100] or v2 != [200] or p2 != (4).as(usize) { return 6 }

            -- Endian readers/writers and `IntoIterator` over `Bytes`
            let endian_buf = Bytes.new()
            endian_buf.write_u16_le((0x1234).as(u16))
            endian_buf.write_u32_be((0xAABBCCDD).as(u32))
            if endian_buf.read_u16_le(0) != (0x1234).as(u16)
                or endian_buf.read_u32_be(2) != (0xAABBCCDD).as(u32) {
                return 6
            }
            var byte_sum: i64 = 0
            for b in Bytes.from_list([10, 20, 30]) {
                byte_sum += b.as(i64)
            }
            if byte_sum != 60 { return 6 }

            -- `PackError` variants
            let err_fmt = bytes.pack_size("z") match {
                _! => false,
                error PackError.InvalidFormat(_) => true,
                error _ => false,
            }
            if not err_fmt { return 7 }

            let err_short = bytes.unpack("<I4", Bytes.from_list([1, 2])) match {
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

            -- UTF-8 helpers on `Bytes`
            let utf8_raw = Bytes.from_string("hé!")
            if utf8_raw.to_list() != [104, 195, 169, 33] { return 11 }
            if utf8_raw.to_utf8() != "hé!" or utf8_raw.to_string_utf8()! != "hé!" { return 12 }
            let invalid_utf8 = Bytes.from_list([255, 65])
            if bytes.to_utf8(invalid_utf8) != null { return 13 }
            let bad_utf8_err = invalid_utf8.to_string_utf8() match {
                _! => false,
                error Utf8Error.InvalidByte(off) => off == (0).as(usize),
            }
            if not bad_utf8_err { return 13 }
            if invalid_utf8.to_utf8_lossy() != "\uFFFD" ++ "A" { return 14 }

            -- Hex roundtrip and `DecodeError` cases
            let hex_sample = Bytes.from_list([0, 15, 16, 255])
            if hex_sample.to_hex() != "000f10ff" or bytes.to_hex(hex_sample) != "000f10ff" { return 15 }
            if Bytes.from_hex("000F10ff")! != hex_sample { return 16 }
            if bytes.from_hex("")! != Bytes.new() { return 17 }
            let hex_odd = Bytes.from_hex("abc") match {
                _! => false,
                error DecodeError.InvalidLength(len) => len == (3).as(usize),
                error _ => false,
            }
            if not hex_odd { return 18 }
            let hex_bad_ch = Bytes.from_hex("0g") match {
                _! => false,
                error DecodeError.InvalidByte(off, ch) => off == (1).as(usize) and ch == 'g',
                error _ => false,
            }
            if not hex_bad_ch { return 19 }

            -- RFC 4648 Base64 vectors and strict `DecodeError` validation
            if Bytes.new().to_base64() != "" { return 20 }
            if Bytes.from_string("f").to_base64() != "Zg==" { return 21 }
            if Bytes.from_string("fo").to_base64() != "Zm8=" { return 22 }
            if Bytes.from_string("foo").to_base64() != "Zm9v" { return 23 }
            if Bytes.from_string("foob").to_base64() != "Zm9vYg==" { return 24 }
            if Bytes.from_string("fooba").to_base64() != "Zm9vYmE=" { return 25 }
            if Bytes.from_string("foobar").to_base64() != "Zm9vYmFy" { return 26 }

            let bin = Bytes.from_list([0, 255, 128, 63, 251, 239])
            let b64 = bytes.to_base64(bin)
            if b64 != "AP+AP/vv" { return 27 }
            if bytes.from_base64(b64)! != bin { return 28 }
            if Bytes.from_base64("Zg==")! != Bytes.from_string("f") { return 29 }
            if Bytes.from_base64("Zm8=")! != Bytes.from_string("fo") { return 30 }
            if Bytes.from_base64("Zm9vYmFy")! != Bytes.from_string("foobar") { return 31 }
            if Bytes.from_base64("")! != Bytes.new() { return 32 }

            -- Reject malformed Base64 with exact `DecodeError` variants
            let b64_len = Bytes.from_base64("abc") match {
                _! => false,
                error DecodeError.InvalidLength(len) => len == (3).as(usize),
                error _ => false,
            }
            if not b64_len { return 33 }

            let b64_mid_pad = Bytes.from_base64("Zg==Zm9v") match {
                _! => false,
                error DecodeError.InvalidPadding => true,
                error _ => false,
            }
            if not b64_mid_pad { return 34 }

            let b64_dirty2 = Bytes.from_base64("Zh==") match {
                _! => false,
                error DecodeError.InvalidPadding => true,
                error _ => false,
            }
            if not b64_dirty2 { return 35 }

            let b64_dirty1 = Bytes.from_base64("Zm9=") match {
                _! => false,
                error DecodeError.InvalidPadding => true,
                error _ => false,
            }
            if not b64_dirty1 { return 36 }

            let b64_bad_byte = Bytes.from_base64("Zm$=") match {
                _! => false,
                error DecodeError.InvalidByte(off, ch) => off == (2).as(usize) and ch == '$',
                error _ => false,
            }
            if not b64_bad_byte { return 37 }

            42
        }

        fn main() -> i64 {
            run_checks() ! { _ => -999 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn math_lua_parity_and_float_formatting() {
    let source = r#"
        fn main() -> i64 {
            if (not INFINITY().is_infinite())
                or (not INFINITY().is_sign_positive())
                or infinity() != INFINITY() {
                return 1
            }
            if (not NEG_INFINITY().is_infinite())
                or (not NEG_INFINITY().is_sign_negative())
                or neg_infinity() != NEG_INFINITY() {
                return 1
            }
            if (not NAN().is_nan()) or PI() != pi() or TAU() != tau() or E() != euler() {
                return 1
            }
            if i64_max() != 9223372036854775807 { return 2 }
            if i64_min() != -9223372036854775807 - 1 { return 3 }

            if (5.5).algebraic_rem(2.0) != 1.5
                or (-5.5).algebraic_rem(2.0) != -1.5
                or (-5.5).rem_euclid(2.0) != 0.5 {
                return 4
            }

            let (i1, f1) = split_fract(3.25)
            if i1 != 3.0 or f1 != 0.25 { return 5 }
            let (i2, f2) = (-3.25).split_fract()
            if i2 != -3.0 or f2 != -0.25 { return 6 }
            let (i3, f3) = split_fract(-3.0)
            if i3 != -3.0 or f3 != 0.0 or (not f3.is_sign_negative()) { return 7 }
            let (i_inf, f_inf) = split_fract(NEG_INFINITY())
            if (not i_inf.is_infinite())
                or (not i_inf.is_sign_negative())
                or f_inf != 0.0
                or (not f_inf.is_sign_negative()) {
                return 8
            }
            let (i_nan, f_nan) = split_fract(NAN())
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

            -- `approx_eq` and endpoint-exact `lerp`
            if (not approx_eq(0.1 + 0.2, 0.3))
                or (not (0.0).approx_eq(1.0e-12, abs_tol = 1.0e-9))
                or approx_eq(NAN(), 0.0)
                or approx_eq(INFINITY(), 1.0e300)
                or (not approx_eq(INFINITY(), INFINITY())) {
                return 17
            }
            if lerp(10.0, 20.0, 0.0) != 10.0
                or lerp(10.0, 20.0, 1.0) != 20.0
                or (10.0).lerp(20.0, 0.25) != 12.5
                or lerp(-4.0, 4.0, 0.5) != 0.0 {
                return 17
            }

            -- Two's-complement unsigned comparison via `cast_unsigned()`
            if (not ((1).cast_unsigned() < (2).cast_unsigned()))
                or (not ((1).cast_unsigned() < (-1).cast_unsigned()))
                or ((-1).cast_unsigned() < (1).cast_unsigned())
                or (not ((-2).cast_unsigned() < (-1).cast_unsigned())) {
                return 18
            }

            -- First-class `Type` inspection via `type_of`
            if type_of(42) != i64
                or type_of((7).as(i8)) != i8
                or type_of((7).as(u32)) != u32
                or type_of((7).as(u128)) != u128 {
                return 19
            }
            if type_of(3.5) != f64 or type_of((3.5).as(f32)) != f32 or type_of("42") != String {
                return 20
            }

            -- Checked float-to-integer truncation (`trunc_i64`, `trunc_u64`)
            if (-42.75).trunc_i64() != -42
                or (42.9).trunc_u64() != (42).as(u64)
                or (-1.0).trunc_u64() != null
                or INFINITY().trunc_i64() != null
                or NAN().trunc_i64() != null
                or (1.0e30).trunc_i64() != null {
                return 21
            }

            42
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

fn rust_xoshiro256ss_reference(seed: u64) -> [u64; 4] {
    fn sm64(state: u64) -> (u64, u64) {
        let next = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = next;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (next, z)
    }
    let (st1, mut s0) = sm64(seed);
    let (st2, mut w1) = sm64(st1);
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
    let seed_val: u64 = 0x1234_5678_9ABC_DEF0;
    let expected = rust_xoshiro256ss_reference(seed_val);
    let source = format!(
        r#"
        use std.collections.*
        use std.rand
        use std.rand.Rng

        fn main() -> i64 {{
            rand.seed()
            rand.seed(value = 777)
            let a1 = rand.next_u64()
            let a2 = rand.next_u64()
            let f = rand.float()
            if f < 0.0 or f >= 1.0 {{ return 1 }}

            rand.seed(value = 777)
            if rand.next_u64() != a1 or rand.next_u64() != a2 {{ return 2 }}

            if rand.bool(probability = 0.0) or (not rand.bool(probability = 1.0)) {{
                return 3
            }}
            let _coin = rand.bool()

            var idx = 0
            while idx < 50 {{
                let v = rand.int_range(-5, 5)
                if v < -5 or v > 5 {{ return 4 }}
                let u = rand.int_range(1, 7)
                if u < 1 or u > 7 {{ return 5 }}
                idx += 1
            }}
            let _span = rand.int_range(i64_min(), i64_max())
            if rand.random_bytes(13).len() != (13).as(usize) {{ return 5 }}

            let items = [10, 20, 30, 40, 50]
            rand.shuffle(items)
            if items.len() != 5 or (not items.contains(10)) or (not items.contains(50)) {{
                return 6
            }}
            if rand.choose(List()) != null {{ return 7 }}
            if not items.contains(rand.choose(items)) {{ return 8 }}

            -- Pure-Nessa Xoshiro256** exact bit-level reference check
            let seed_u64: u64 = {seed_val}
            let rng = Rng.from_seed(seed_u64)
            let r0: u64 = {}
            let r1: u64 = {}
            let r2: u64 = {}
            let r3: u64 = {}
            if rng.next_u64() != r0 {{ return 9 }}
            if rng.next_u64() != r1 {{ return 10 }}
            if rng.next_u64() != r2 {{ return 11 }}
            if rng.next_u64() != r3 {{ return 12 }}

            -- Capability effect `rand.random_u64` via `with_seeded_rng` and `with_rng`
            let (eff0, eff1): (u64, u64) = rand.with_seeded_rng(seed_u64, || -> #rand.random_u64 (u64, u64) {{
                let x = rand.random_u64()#
                let y = rand.random_u64()#
                (x, y)
            }})
            let explicit_rng = Rng.new(seed = seed_u64)
            let eff_first: u64 = rand.with_rng(explicit_rng, || -> #rand.random_u64 u64 {{
                rand.random_u64()#
            }})
            if eff0 != r0 or eff1 != r1 or eff_first != r0 {{ return 12 }}

            let rf = rng.float()
            if rf < 0.0 or rf >= 1.0 {{ return 13 }}
            if rng.int_range(99, 99) != 99 {{ return 14 }}
            let _full_rng = rng.int_range(i64_min(), i64_max())
            if rng.bool(probability = 0.0) or (not rng.bool(probability = 1.0)) {{ return 14 }}
            if rng.bytes(11).len() != (11).as(usize) {{ return 14 }}

            let deck = ["a", "b", "c", "d"]
            rng.shuffle(deck)
            if deck.len() != 4 or (not deck.contains("a")) or (not deck.contains("d")) {{
                return 15
            }}
            if rng.choose(List()) != null or (not deck.contains(rng.choose(deck))) {{
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
        fn main() -> i64 { rand.int_range(10, 5) }
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

            let f_nan = NAN().as(f32)
            let f_inf = INFINITY().as(f32)
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
        use std.bytes
        use std.bytes.PackError
        use std.fmt
        use std.fmt.FormatError
        use std.pattern
        use std.pattern.PatternError

        fn collect_texts(text: String, pat: String) -> !PatternError List {
            let it = pattern.find_iter(text, pat)!
            let out = List()
            for m in it {
                out.push(m.slice_of(text))
            }
            out
        }

        fn invalid_utf8_offset(raw: List) -> ?usize {
            Bytes.from_list(raw).validate_utf8() match {
                _! => null,
                error Utf8Error.InvalidByte(off) => off,
            }
        }

        fn run_checks() -> ![PatternError, FormatError, PackError, Utf8Error] i64 {
            -- 1. `%BHH` UTF-8 byte-range pattern & `chars_iter()` keep U+0080..=U+00BF scalars ('©', '®') and '\n' separate
            let utf8_pat = "[%B00-%B7F%BC2-%BF4][%B80-%BBF]*"
            if collect_texts("a©b", utf8_pat)! != ["a", "©", "b"] {
                return 2
            }
            if collect_texts("©®", utf8_pat)! != ["©", "®"]
                or "©®".chars_iter().collect() != ['©', '®'] {
                return 3
            }
            if collect_texts("a\nb", utf8_pat)! != ["a", "\n", "b"] {
                return 4
            }
            if collect_texts("中😀!", utf8_pat)! != ["中", "😀", "!"] {
                return 5
            }
            -- Standalone `%BHH` escape matches exact UTF-8 lead/continuation bytes of '©' (0xC2 0xA9)
            let copy_opt = pattern.find("x©y", "%BC2%BA9")!
            let copy_hit? = copy_opt else { return 6 }
            if copy_hit.slice_of("x©y") != "©" {
                return 6
            }
            let bad_hex = pattern.find("abc", "%BZZ") match {
                _! => false,
                error PatternError.InvalidPattern(_) => true,
            }
            if not bad_hex {
                return 7
            }

            -- 2. 0-based `char_slice`, `byte_slice`, `char_indices_iter`, `char_at_byte`, and `from_chars`
            if "a©b🎉z".char_slice(1, end = 4) != "©b🎉" {
                return 13
            }
            if "abcdef".char_slice(3) != "def" or "abcdef".char_slice(0, end = 0) != "" {
                return 14
            }
            if "a©".byte_slice(1, end = 3) != "©"
                or "a©".byte_slice(1, end = 2) != null
                or "a©".to_bytes().slice(1, 3).to_list() != [0xC2, 0xA9] {
                return 15
            }
            if from_chars(['N', '©', '🎉']) != "N©🎉" {
                return 16
            }

            let pairs = List()
            for (byte_off, ch) in "a©b".char_indices_iter() {
                pairs.push((byte_off.as(i64), ch.to_code()))
            }
            if pairs != [(0, 97), (1, 169), (3, 98)] {
                return 17
            }
            if "a©b".char_at_byte(0) != 'a'
                or "a©b".char_at_byte(1) != '©'
                or "a©b".char_at_byte(2) != null
                or "a©b".char_at_byte(3) != 'b' {
                return 18
            }

            -- 3. Strict RFC 3629 `Bytes.validate_utf8` returning scalar count or `Utf8Error.InvalidByte(offset)`
            let ok_cnt = Bytes.from_string("a©🎉").validate_utf8()!
            if ok_cnt != (3).as(usize) {
                return 19
            }
            -- Mid-character continuation byte slice from "a©b" (bytes 2..4 = [0xA9, 0x62]) fails at offset 0
            let mid_raw = Bytes.from_string("a©b").slice(2, 4)
            let mid_err = mid_raw.validate_utf8() match {
                _! => false,
                error err => err == Utf8Error.InvalidByte((0).as(usize))
                    and err.to_string().contains("offset 0"),
            }
            if not mid_err {
                return 20
            }
            -- Overlong 2-byte NUL (0xC0 0x80), 3-byte overlong '/' (0xE0 0x80 0xAF),
            -- UTF-16 surrogate U+D800 (0xED 0xA0 0x80), > U+10FFFF (0xF4 0x90 0x80 0x80),
            -- and truncated 2-byte sequence ([0x61, 0xC2]) must all fail at exact byte offset!
            if invalid_utf8_offset([0x61, 0xC0, 0x80]) != (1).as(usize) { return 21 }
            if invalid_utf8_offset([0xE0, 0x80, 0xAF]) != (0).as(usize) { return 22 }
            if invalid_utf8_offset([0x61, 0x62, 0xED, 0xA0, 0x80]) != (2).as(usize) { return 23 }
            if invalid_utf8_offset([0xF4, 0x90, 0x80, 0x80]) != (0).as(usize) { return 24 }
            if invalid_utf8_offset([0x61, 0xC2]) != (1).as(usize) { return 25 }

            -- 4. `%p` pointer formatting: heap objects format as `0x...`, immediates & numbers as `(null)`
            let lst = [1, 2]
            let p_lst = format("%p", lst)!
            if (not p_lst.starts_with("0x")) or p_lst.len() < 3 {
                return 26
            }
            let p_pad = format("%-24p", lst)!
            if p_pad.len() != 24 or (not p_pad.starts_with(p_lst)) {
                return 27
            }
            let big_i128: i128 = i128_max()
            if format("%p|%p|%p|%p|%p", null, 42, 3.5, true, big_i128)!
                != "(null)|(null)|(null)|(null)|(null)" {
                return 28
            }
            let bad_p = fmt.format("%.4p", lst) match {
                _! => false,
                error FormatError.InvalidSpec(_) => true,
                error _ => false,
            }
            if not bad_p {
                return 29
            }

            -- 5. `bytes.pack` / `bytes.unpack` / `bytes.pack_size` with `Xs` and `Xs2` / `Xs4` alignment
            let sz_unaligned = bytes.pack_size("<bXs4b")!
            let sz_aligned4 = bytes.pack_size("<!4bXs4b")!
            let sz_aligned8 = bytes.pack_size("<!8bXsb")!
            if sz_unaligned != 2 or sz_aligned4 != 5 or sz_aligned8 != 9 {
                return 31
            }
            let packed = bytes.pack("<!4bXs2s2", 7, "hi")!
            if packed != Bytes.from_list([7, 0, 2, 0, 104, 105]) {
                return 33
            }
            let (vals, next_pos) = bytes.unpack("<!4bXs2s2", packed)!
            if next_pos != (6).as(usize) or vals != [7, "hi"] {
                return 35
            }

            42
        }

        fn main() -> i64 {
            run_checks() ! { _ => -999 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}
