//! Compile and execute in distinct processes, with the source removed before run.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "nessa-archive-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("cannot create test directory: {error}"),
            }
        }
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn cli(command: &str, path: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_nessa"))
        .arg(command)
        .arg(path)
        .output()
        .unwrap()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status: {}; stdout: {}; stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn roundtrip(source: &str, expected: &str) {
    let directory = TestDirectory::new();
    let path = directory.0.join("program.ns");
    std::fs::write(&path, source).unwrap();
    let source_run = cli("run", &path);
    assert_success(&source_run);
    assert_eq!(String::from_utf8(source_run.stdout).unwrap(), expected);
    assert_success(&cli("build", &path));
    let archive = path.with_extension("nsbc");
    assert!(archive.is_file());
    std::fs::remove_file(path).unwrap();
    // A second execution process starts with a fresh interner and type pool.
    let archive_run = cli("run", &archive);
    assert_success(&archive_run);
    assert_eq!(String::from_utf8(archive_run.stdout).unwrap(), expected);
}

#[test]
fn archive_preserves_source_factory_identity_and_tagged_null_payload() {
    roundtrip(
        "use std.builtin.IterationStep as Step\ntypealias S=Step(Any)\nfn next()->S{S.yielded(null)}\nfn main(){let result=next();print(if result'type==Step(Any) and S.done'type==S {result match {S.yielded(null)=>42,S.done=>0,_=>1}}else{0})}",
        "42",
    );
    roundtrip(
        "typealias S=IterationStep(String)\nfn next()->S{S.yielded(\"typed payload\")}\nfn main(){next() match {S.yielded(value)=>print(value),S.done=>print(\"done\")}}",
        "typed payload",
    );
    roundtrip(
        "impl IterationStep(i64){pub fn answer(self)->i64{self match{IterationStep(i64).yielded(n)=>n,_=>0}}}\nfn main(){println(IterationStep(i64).yielded(42).answer())}",
        "42\n",
    );
}

#[test]
fn archive_preserves_dependent_iteration_defaults_and_scoped_items() {
    roundtrip(
        "struct P{x:i64};trait Source{assoc Item:Type=Self;assoc Result:Type=IterationStep(Item);derive fn next(self)->Result{IterationStep(Item).yielded(self)}};impl Source for P{};fn main(){P{x:42}.next() match{IterationStep(P).yielded(value)=>println(value.x),_=>println(0)}}",
        "42\n",
    );
    roundtrip(
        "struct P{};trait Source{assoc Item:Type=Any;derive fn next(self,value:Item)->IterationStep(Item){IterationStep(Item).yielded(value)}};mod a{extend Source for P{assoc Item:Type=String};pub fn next()->IterationStep(String){P{}.next(\"abcdefgh\")}};mod b{extend Source for P{assoc Item:Type=i64};pub fn answer()->i64{let text=a.next() match{IterationStep(String).yielded(text)=>text.len(),_=>0};let n=P{}.next(34) match{IterationStep(i64).yielded(n)=>n,_=>0};text+n}};fn main(){println(b.answer())}",
        "42\n",
    );
    roundtrip(
        "struct P{};trait Source{assoc Item:Type=i64;assoc typealias S=IterationStep(Item);derive fn nested(self,value:Item)->(?S,fn()->S){(S.yielded(value),||->S{S.yielded(value)})};derive fn result_type(self)->Type{S}};impl Source for P{};fn main(){let p=P{};let result=p.nested(21);let a=result.0.as(IterationStep(i64)) match{IterationStep(i64).yielded(n)=>n,_=>0};let b=result.1() match{IterationStep(i64).yielded(n)=>n,_=>0};println(if p.result_type()==IterationStep(i64){a+b}else{0})}",
        "42\n",
    );
}

#[test]
fn enum_default_fields_and_variadic_lists_execute_after_source_deletion() {
    roundtrip(
        "global trace:i64=0;fn step(n:i64)->i64{trace=trace*10+n;n};enum E{value(.a:i64=step(1),.b:i64=a+step(2),.c:i64=step(3))};fn main(){E.value(c=step(9)) match{E.value(a,b,c)=>{println(trace);println((a,b,c))}}}",
        "912\n(1, 3, 9)\n",
    );
    roundtrip(
        "enum E{packet(text:String,...items:List,.f:fn()->String=||text,.size:i64=items.len())};fn make()->E{E.packet(\"abcd\"++\"efgh\",null,(),\"ijkl\"++\"mnop\")};fn main(){make() match{E.packet(text,items,f,size)=>{println(f().len()+items(2).as(String).len()+26);println(size==3 and items(0)==null and items(1)==())}}}",
        "42\ntrue\n",
    );
    roundtrip(
        "enum E{value(.n:i64=storage.n)};mod api{pub global answer:E=E.value()};mod storage{pub global n:i64=42};fn main(){api.answer match{E.value(n)=>println(n)}}",
        "42\n",
    );
    roundtrip(
        "enum E{value(n:i64,.m:i64=n+2)};struct P{};trait Read{assoc Item:Type=i64;fn item(self)->Item;derive fn packet(self)->E{E.value(self.item())}};impl Read for P{pub fn item(self)->i64{40}};fn main(){P{}.packet() match{E.value(n,m)=>println(m)}}",
        "42\n",
    );
    let result =
        driver::Driver::new().compile("enum E{value(x:i64,.y:i8=2)};fn main(){E.value(40)}");
    assert!(!result.has_errors, "{:?}", result.diagnostics);
    let artifact = result.into_artifact().unwrap();
    let bytes = nsbc_io::write_artifact(&artifact).unwrap();
    let restored = nsbc_io::read_artifact(&bytes).unwrap();
    let find_fields = |pool: &type_pool::TypePool| {
        pool.snapshot()
            .types
            .into_iter()
            .find_map(|info| match info.kind {
                type_pool::TypeKind::Enum { name, variants } if str_interner::get(name) == "E" => {
                    Some(variants[0].fields.clone())
                }
                _ => None,
            })
            .unwrap()
    };
    let fields = find_fields(&artifact.type_pool);
    assert!(!fields[0].has_default);
    assert!(fields[1].has_default);
    assert_eq!(
        artifact.type_pool.canonical_type(fields[1].ty),
        Some(type_pool::Intrinsic::I8.type_index())
    );
    let describe = |fields: &[type_pool::FieldInfo]| {
        fields
            .iter()
            .map(|field| (field.name, field.ty, field.has_default, field.offset))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        describe(&fields),
        describe(&find_fields(&restored.type_pool))
    );
    assert_eq!(bytes, nsbc_io::write_artifact(&restored).unwrap());
    roundtrip(
        "fn wrong()->Any{true};enum E{value(.n:i64=wrong())};fn main(){E.value(n=42) match{E.value(n)=>println(n)}}",
        "42\n",
    );
    roundtrip(
        "effect supply()->i64;enum E{value(.n:i64=supply()#{supply()=>if true{return 42}else{0}})};fn main(){E.value() match{E.value(n)=>println(n)}}",
        "42\n",
    );
}

#[test]
fn enum_default_control_boundaries_reject_cli_artifacts() {
    for source in [
        "enum E{value(.n:i64=true match{true=>if true{return 42}else{0},_=>0})};fn main(){E.value() match{E.value(n)=>n}}",
        "fn value(.n:i64=true match{true=>if true{return 42}else{0},_=>0})->i64{n};fn main(){value()}",
        "struct Value{n:i64=true match{true=>if true{return 42}else{0},_=>0}};fn main(){Value{}}",
    ] {
        let directory = TestDirectory::new();
        let path = directory.0.join("invalid-default-return.ns");
        std::fs::write(&path, source).unwrap();
        let output = cli("build", &path);
        assert!(!output.status.success());
        assert!(!path.with_extension("nsbc").exists());
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("default boundary"));
    }
}

#[test]
fn enum_default_any_payload_errors_survive_archive_loading() {
    let directory = TestDirectory::new();
    let path = directory.0.join("enum-default-any.ns");
    std::fs::write(
        &path,
        "fn wrong()->Any{true};enum E{value(.n:i64=wrong())};fn main(){E.value()}",
    )
    .unwrap();
    for archive in [false, true] {
        if archive {
            assert_success(&cli("build", &path));
            std::fs::remove_file(&path).unwrap();
        }
        let output = cli(
            "run",
            &if archive {
                path.with_extension("nsbc")
            } else {
                path.clone()
            },
        );
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("TypeError"));
    }
}

#[test]
fn map_snapshot_iteration_executes_without_source_and_rejects_older_native_capabilities() {
    roundtrip(
        "fn main(){let m=Map();m(\"a\")=20;m(\"b\")=22;m(\"null\")=null;let iter=m.into_iter();m.remove(\"a\");m(\"b\")=99;m(\"new\")=100;let sum:i64=0;let nulls:i64=0;for (key,value) in iter{if value==null{nulls+=1}else{sum+=value.as(i64)}};println(sum);println(nulls);println(iter.next()'type==IterationStep((String,Any)));println(iter.next() match{IterationStep((String,Any)).done=>true,_=>false})}",
        "42\n1\ntrue\ntrue\n",
    );
    roundtrip(
        "fn main(){let m=Map();m(\"null\")=null;m(\"unit\")=();m(\"answer\")=42;let keys=m.keys();let vals=m.values();let entries=m.entries();m.remove(\"null\");m(\"answer\")=99;let count:i64=0;for entry in entries{let pair:(String,Any)=entry.as((String,Any));if pair.0==\"answer\"{println(pair.1)};count+=1};println(keys.len()==3 and vals.len()==3 and count==3)}",
        "42\ntrue\n",
    );
    let compiled = driver::Driver::new().compile("fn main(){println(Map().keys().len())}");
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let artifact = compiled.into_artifact().unwrap();
    assert!(
        artifact
            .builtins
            .iter()
            .any(|import| import.id == 126 && import.name == "__map_keys")
    );
    let directory = TestDirectory::new();
    for revision in 1..=7 {
        let mut bad = nsbc_io::read_artifact(&nsbc_io::write_artifact(&artifact).unwrap()).unwrap();
        bad.builtin_abi_version = revision;
        let path = directory.0.join(format!("old-map-keys-{revision}.nsbc"));
        std::fs::write(&path, nsbc_io::write_artifact(&bad).unwrap()).unwrap();
        let output = cli("run", &path);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("incompatible builtin ABI"));
    }
}

#[test]
fn archive_preserves_single_next_loops_snapshots_and_default_targets() {
    roundtrip(
        "global calls:i64=0;struct Counter{n:i64};impl Iterator for Counter{assoc Item:Type=i64;pub fn next(self)->IterationStep(i64){calls+=1;if self.n<3{self.n=self.n+1;IterationStep(i64).yielded(self.n)}else{IterationStep(i64).done}}};fn main(){let sum:i64=0;let iter=Counter{n:0};for n if n!=2 in iter{sum+=n};println(if sum==4 and calls==4{42}else{0})}",
        "42\n",
    );
    roundtrip(
        "fn main(){let values=[null,20,22];let iter=values.into_iter();values.pop();values.set(0,99);let total:i64=0;for item if item!=null in iter{total+=item.as(i64)};println(total)}",
        "42\n",
    );
    roundtrip(
        "trait Total{derive fn total(self)->i64{let iter:Self=self;let sum:i64=0;for item in iter{sum+=item};sum}};struct Cursor{done:bool};mod a{extend Iterator for Cursor{assoc Item:Type=i64;pub fn next(self)->IterationStep(i64){if self.done{IterationStep(i64).done}else{self.done=true;IterationStep(i64).yielded(40)}}};extend Total for Cursor{};pub fn answer()->i64{Cursor{done:false}.total()}};mod b{extend Iterator for Cursor{assoc Item:Type=i64;pub fn next(self)->IterationStep(i64){if self.done{IterationStep(i64).done}else{self.done=true;IterationStep(i64).yielded(2)}}};extend Total for Cursor{};pub fn answer()->i64{Cursor{done:false}.total()}};fn main(){println(a.answer()+b.answer())}",
        "42\n",
    );
}

#[test]
fn archive_preserves_inherited_self_calls_through_child_trait_parameters() {
    roundtrip(
        "trait Base{fn clone(self)->Self}\ntrait Child(Base){fn value(self)->i64}\ntypealias View=Child\nstruct P{x:i64}\nimpl Base for P{pub fn clone(self)->Self{self}}\nimpl Child for P{pub fn value(self)->i64{self.x}}\nfn copied(x:View)->Child{x.clone()}\nfn answer(x:Child)->i64{copied(x).value()}\nfn main(){println(answer(P{x:42}))}",
        "42\n",
    );
}

#[test]
fn archive_keeps_receiver_alias_initialization_dependencies_frozen() {
    for body in [
        "let copy=self;copy.next()",
        "let first=self;let second:Self=first;second.next()",
    ] {
        let source = format!(
            "struct P{{}};trait Source{{assoc Item:Type=i64;fn next(self)->Item;derive fn copy(self)->Item{{{body}}}}};mod api{{extend Source for P{{pub fn next(self)->i64{{storage.value}}}};pub global answer:i64=P{{}}.copy()}};mod unused{{pub global value:i64=api.answer;extend Source for P{{pub fn next(self)->i64{{value}}}}}};mod storage{{pub global value:i64=42}};fn main(){{println(api.answer)}}"
        );
        roundtrip(&source, "42\n");
    }
}

#[test]
fn archive_runs_module_initializers_once_and_uses_saved_startup_entry() {
    roundtrip(
        "global count: i64 = 0\nmod api { pub global value: i64 = 1; fn __init__() { count += 1; value += 40; }; pub fn answer() -> i64 { value + 1 } }\nuse api.answer as first\nuse api.answer as second\nfn main() { println(count); println(first()); println(second()); }",
        "1\n42\n42\n",
    );
}

#[test]
fn archive_preserves_type_values_function_signatures_and_capturing_calls() {
    roundtrip(
        "typealias Count = i64\nfn apply(f: fn(i64) -> i64) -> i64 { f(40) }\nfn main() { let delta = 2; let f = |x: i64| x + delta; println(apply(f)); println(f'type == fn(i64) -> i64); println(type_of(42) == Count); println(Count); println(null); }",
        "42\ntrue\ntrue\ni64\nnull\n",
    );
}

#[test]
fn archive_preserves_typed_native_adapters_and_exact_numeric_payloads() {
    roundtrip(
        "fn apply(f: fn(Any) -> i64) -> i64 { f(42) }\nfn main() { println(apply(to_i64)); let min: i128 = -170141183460469231731687303715884105728; let max: u128 = 340282366920938463463374607431768211455; println(min); println(max); println(pow(2, 63)); }",
        "42\n-170141183460469231731687303715884105728\n340282366920938463463374607431768211455\n9223372036854775808\n",
    );
}

#[test]
fn archive_relocates_method_names_and_preserves_derived_dispatch() {
    roundtrip(
        "struct Boxed { value: i64 }\nderive Eq for Boxed\nfn main() { let a = Boxed { value: 42 }; let b = Boxed { value: 42 }; println(a.eq(b)); }",
        "true\n",
    );
}

#[test]
fn archive_method_relocation_works_with_existing_interner_ids_above_4095() {
    let directory = TestDirectory::new();
    let path = directory.0.join("program.ns");
    std::fs::write(&path, "struct Boxed { value: i64 }\nderive Eq for Boxed\nfn main() { let a = Boxed { value: 42 }; let b = Boxed { value: 42 }; println(a.eq(b)); }").unwrap();
    assert_success(&cli("build", &path));
    std::fs::remove_file(&path).unwrap();
    let child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "relocation_child", "--nocapture"])
        .env("NESSA_ARCHIVE_RELOCATION_TEST", path.with_extension("nsbc"))
        .output()
        .unwrap();
    assert_success(&child);
    let stdout = String::from_utf8(child.stdout).unwrap();
    assert!(stdout.lines().any(|line| line == "true"), "{stdout}");
}

#[test]
fn relocation_child() {
    let Some(path) = std::env::var_os("NESSA_ARCHIVE_RELOCATION_TEST") else {
        return;
    };
    for index in 0..5000 {
        str_interner::intern(&format!("unrelated-existing-name-{index}"));
    }
    let result = driver::Driver::new().run_archive_file(Path::new(&path));
    assert!(matches!(result, driver::RunResult::Ok), "{result:?}");
    // The narrow method-name encoding could not hold the restored ID. The
    // artifact's dedicated far operand must carry the newly interned name.
    assert!(str_interner::intern("eq").as_u32() >= 5000);
}

#[test]
fn archive_preserves_effect_identity_and_multiple_continuation_branches() {
    roundtrip(
        "effect choose(catch k) -> i64\nfn compute() { let value = choose()#; value * 10 }\nfn main() { let result = compute()# { choose(k) => k(1) + k(2) }; println(result); }",
        "30\n",
    );
}

#[test]
fn archive_preserves_far_jumps_over_large_conditional_bodies() {
    let source = format!(
        "global count: i64 = 0\nfn main() {{ if false {{ {} }}; println(count); }}",
        "count = 1;\n".repeat(33000),
    );
    roundtrip(&source, "0\n");
}

#[test]
fn corrupt_archives_fail_before_running_any_code() {
    let directory = TestDirectory::new();
    let path = directory.0.join("program.ns");
    std::fs::write(&path, "fn main() { println(42); }").unwrap();
    assert_success(&cli("build", &path));
    let archive = path.with_extension("nsbc");
    let original = std::fs::read(&archive).unwrap();
    for bytes in [b"NSBC".as_slice(), &original[..original.len() - 1]] {
        std::fs::write(&archive, bytes).unwrap();
        let output = cli("run", &archive);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains("archive load failed"), "{stderr}");
        assert!(!stderr.contains("panicked"), "{stderr}");
    }
}

#[test]
fn archive_preserves_named_arguments_and_declaration_scope_defaults() {
    roundtrip(
        "mod defaults { pub global base: i64 = 40 }\nmod api { pub fn value(a: i64, .b: i64 = a + defaults.base) -> i64 { b } }\nconst startup = api.value(a = 2)\nfn main() { println(startup); println(api.value(b = 42, a = 0)); var x: i64 = 1; let pair = |a, b| a * 10 + b; println(pair(x, if true { x = 2; x } else { 0 })); }",
        "42\n42\n12\n",
    );
}

#[test]
fn archive_preserves_checked_struct_layout_defaults_and_startup_dependencies() {
    roundtrip(
        "mod defaults { pub global base: i64 = 2 }\nstruct Pair { a: i64, b: i64 = defaults.base }\ntypealias Alias = Pair\nconst shared = Alias { a: 4 }\nfn main() { println(shared.a * 10 + shared.b); let a: i64 = 4; let b: i64 = 2; let p = Pair { b, a }; println(p.a * 10 + p.b); }",
        "42\n42\n",
    );
}

#[test]
fn archive_runs_plain_impl_constructors_and_associated_initializers() {
    roundtrip(
        "var count: i64 = 0\nstruct Pair { value: i64 }\nimpl Pair { pub global base: i64 = 40; fn __init__() { count = count + 1 }; pub fn new(.value: i64 = base + 2) -> Pair { Pair { value: value } } }\nimpl Pair { pub fn answer() -> i64 { Pair.base + 2 } }\ntypealias Alias = Pair\nfn main() { println(Alias().value); println(Pair.answer()); println(count); }",
        "42\n42\n1\n",
    );
}

#[test]
fn archive_preserves_private_receiver_fields_and_alias_module_hooks() {
    roundtrip(
        "global count: i64 = 0\nstruct Point { private x: i64 }\nimpl Point { pub fn new(.x: i64 = 42) -> Point { Self { x } }; pub fn value(self) -> i64 { self.x } }\nmod alias { pub typealias Alias = Point; fn __init__() { count = 42 } }\nuse alias.Alias\nfn main() { println(Alias().value()); println(count); }",
        "42\n42\n",
    );
}

#[test]
fn archive_preserves_lists_native_methods_variadic_packing_and_shared_updates() {
    roundtrip(
        "fn collect(...args: List) -> i64 { args(0) + args(1) }\nconst shared = [40, 0]\nfn main() { let alias = shared; alias(1) = 2; println(shared(0) + shared(1)); alias.push(\"abcdefgh\"); println(str_len(alias.pop())); println(alias.len()); println(collect(40, 2)); let empty = List(); println(empty.pop()); }",
        "42\n8\n2\n42\nnull\n",
    );
}

#[test]
fn archive_lists_handle_more_elements_than_the_new_list_immediate_or_call_window() {
    let elements = std::iter::repeat_n("2", 4100).collect::<Vec<_>>().join(",");
    roundtrip(
        &format!(
            "fn count(...args: List) -> i64 {{ args.len() }}\nfn main() {{ let xs = [{elements}]; println(xs.len()); println(xs(4099)); println(count({})); }}",
            std::iter::repeat_n("0", 40).collect::<Vec<_>>().join(",")
        ),
        "4100\n2\n40\n",
    );
}

#[test]
fn archive_preserves_nested_list_display_and_dynamic_indexing() {
    roundtrip(
        "fn main() { println([1, \"hello\", [true, null, ()]]); let xs: Any = [0]; let index: Any = 0; xs(index) = 42; println(xs(index)); let cyclic = [null]; cyclic(0) = cyclic; println(cyclic); }",
        "[1, \"hello\", [true, null, ()]]\n42\n[<cycle>]\n",
    );
}

#[test]
fn archive_preserves_tuple_layout_bindings_and_gradual_boundaries() {
    roundtrip(
        "typealias Pair = (i64, String)\nconst shared: Pair = (0, \"hello\")\nfn change(t: Pair) { t.0 = 42 }\nfn sum((a, b): (i64, i64)) -> i64 { a + b }\nfn main() { change(shared); let (answer, text) = shared; println(answer); println(text); println(shared); let raw: Any = (40, 2); println(sum(raw)); println(([1, 2], (true, null), ())); }",
        "42\nhello\n(42, \"hello\")\n42\n([1, 2], (true, null), ())\n",
    );
}

#[test]
fn archive_preserves_nominal_enum_values_recursive_payloads_and_patterns() {
    roundtrip(
        "enum Tree { leaf(value: i64), node(left: Tree, right: Tree), empty, pub fn total(self) -> i64 { self match { Tree.leaf(n) => n, Tree.node(a, b) => a.total() + b.total(), Tree.empty => 0 } } }\ntypealias Alias = Tree\nconst shared = Alias.node(Tree.leaf(40), Tree.leaf(2))\nenum Other { empty }\nfn main() { println(Tree.empty); println(Tree.leaf(42)); println(shared); println(shared.total()); println(Tree.empty.total()); println(shared match { Tree.node(Tree.leaf(a), Tree.leaf(b)) => a + b, _ => 0 }); println(Tree.empty == Other.empty); let value: Any = Tree.leaf(42); println(value match { Tree.empty => 0, Tree.leaf(n) if n > 40 => n, _ => 0 }); println((Tree.empty, [Tree.leaf(42)])); }",
        "Tree.empty\nTree.leaf(42)\nTree.node(Tree.leaf(40), Tree.leaf(2))\n42\n0\n42\nfalse\n42\n(Tree.empty, [Tree.leaf(42)])\n",
    );
}

#[test]
fn archive_preserves_generated_enum_equality_and_custom_field_dispatch() {
    roundtrip(
        "global calls: i64 = 0\nstruct Key { value: i64 }\nimpl Eq for Key { pub fn eq(self, other: Key) -> bool { calls += 1; self.value % 10 == other.value % 10 } }\nenum Entry { empty, item(key: Key, value: i64) }\nderive Eq for Entry\ntypealias Alias = Entry\nfn main() { let a = Alias.item(Key { value: 42 }, 7); let b = Entry.item(Key { value: 2 }, 7); println(a == b); println(calls); println(a == Entry.item(Key { value: 3 }, 7)); println(calls); println(Entry.empty == a); println(calls); println(a == Entry.item(Key { value: 2 }, 8)); println(calls); }",
        "true\n1\nfalse\n2\nfalse\n2\nfalse\n3\n",
    );
}

#[test]
fn archive_preserves_string_keyed_maps_updates_and_null_membership() {
    roundtrip(
        "const shared = Map()\nfn main() { shared.set(\"first\", 40); shared(\"second\") = 2; println(shared(\"first\") + shared.get(\"second\")); let alias = shared; alias.set(\"first\", 41); println(shared.get(\"first\")); shared.set(\"雪\", null); println(shared.contains(\"雪\")); println(shared.get(\"雪\")); println(shared.contains(\"missing\")); println(shared.get(\"missing\")); println(shared.len()); println(shared.remove(\"second\")); println(shared.contains(\"second\")); println(shared.remove(\"missing\")); println(shared.len()); shared.set(\"nested\", [1, (true, \"hello\")]); println(shared.get(\"nested\")); }",
        "42\n41\ntrue\nnull\nfalse\nnull\n3\n2\nfalse\nnull\n2\n[1, (true, \"hello\")]\n",
    );
}

#[test]
fn archive_preserves_unicode_string_concat_and_custom_result_types() {
    roundtrip(
        r#"global trace: i64 = 0
        global calls: i64 = 0
        struct Piece { value: i64 }
        impl Piece {
            pub fn concat(self, other: Piece) -> i64 {
                calls += 1; self.value + other.value
            }
        }
        fn piece(order: i64) -> Piece {
            trace = trace * 10 + order
            Piece { value: if order == 1 { 40 } else { 2 } }
        }
        fn main() {
            println("雪" ++ "λ")
            println(("你" ++ "好").len())
            println("" ++ "")
            println("左" ++ "" ++ "右")
            let result: i64 = piece(1) ++ piece(2)
            println(result); println(trace); println(calls)
            println([] ++ [40, 2])
            let original = [40, 2]
            let doubled = original ++ original
            println(doubled); println(original.len())
            doubled.push(0)
            println(original.len()); println(doubled.len())
            let inner = [0]
            let map = Map()
            map("value") = 0
            let inputs = [inner, map]
            let combined = inputs ++ []
            combined(0)(0) = 42
            combined(1)("value") = 42
            println(inner(0)); println(map("value"))
            combined.push(0)
            println(inputs.len()); println(combined.len())
        }"#,
        "雪λ\n6\n\n左右\n42\n12\n1\n[40, 2]\n[40, 2, 40, 2]\n2\n2\n5\n42\n42\n2\n3\n",
    );
}

#[test]
fn archived_dynamic_string_concat_rejects_non_string_operands() {
    let directory = TestDirectory::new();
    let path = directory.0.join("program.ns");
    std::fs::write(
        &path,
        "fn main() { let left: Any = \"hello\"; let right: Any = 42; println(left ++ right); }",
    )
    .unwrap();
    assert_success(&cli("build", &path));
    let archive = path.with_extension("nsbc");
    let source_run = cli("run", &path);
    std::fs::remove_file(path).unwrap();
    for output in [source_run, cli("run", &archive)] {
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains("TypeError"), "{stderr}");
        assert!(!stderr.contains("panicked"), "{stderr}");
    }
}

#[test]
fn archive_preserves_effectful_custom_concat_continuation() {
    roundtrip(
        "effect choose(catch k) -> i64\nstruct Piece { value: i64 }\nimpl Piece { pub fn concat(self, other: Piece) -> i64 { let extra = choose()#; self.value + other.value + extra } }\nfn main() { let left = Piece { value: 40 }; let right = Piece { value: 1 }; let answer = (left ++ right)# { choose(k) => k(1) }; println(answer); }",
        "42\n",
    );
}

#[test]
fn archive_preserves_custom_multi_index_apply_update_and_enum_apply() {
    roundtrip(
        r#"global trace: i64 = 0
        struct Grid { value: i64 }
        impl Grid {
            pub fn apply(self, row: i64, column: i64) -> String {
                if row + column == self.value { "match" } else { "miss" }
            }
            pub fn update(self, row: i64, column: i64, value: i64) {
                self.value = row + column + value
            }
        }
        fn index(value: i64) -> i64 { trace = trace * 10 + value; value }
        fn assigned() -> i64 { trace = trace * 10 + 3; 40 }
        enum State {
            ready, loaded(value: i64),
            pub fn apply(self) -> i64 {
                self match { State.ready => 41, State.loaded(value) => value }
            }
        }
        fn main() {
            let grid = Grid { value: 42 }
            println(grid(40, 2)); println(grid(0, 1))
            grid(index(1), index(2)) = assigned()
            println(trace); println(grid.value); println(grid(40, 3))
            let ready = State.ready
            let loaded = State.loaded(42)
            println(ready()); println(loaded())
        }"#,
        "match\nmiss\n123\n43\nmatch\n41\n42\n",
    );
}

#[test]
fn archive_custom_apply_initialization_depends_on_the_method_body() {
    roundtrip(
        r#"mod dependency {
            pub global base: i64 = 0
            fn __init__() { base = 40 }
        }
        struct Lookup { value: i64 }
        impl Lookup {
            pub fn apply(self, delta: i64) -> i64 { dependency.base + self.value + delta }
        }
        const lookup = Lookup { value: 1 }
        const answer = lookup(1)
        fn main() { println(answer); println(dependency.base) }"#,
        "42\n40\n",
    );
}

#[test]
fn archive_preserves_effectful_and_dynamic_custom_apply_update() {
    roundtrip(
        r#"effect choose(catch k) -> i64
        struct Cell { value: i64 }
        impl Cell {
            pub fn apply(self, delta: i64) -> i64 {
                let extra = choose()#
                self.value + delta + extra
            }
            pub fn update(self, delta: i64, value: i64) { self.value = value + delta }
        }
        fn main() {
            let cell = Cell { value: 40 }
            let answer = cell(1)# { choose(k) => k(1) }
            println(answer)
            let dynamic: Any = cell
            dynamic(1) = 41
            println(cell.value)
            let resumed = dynamic(0)# { choose(k) => k(0) }
            println(resumed)
        }"#,
        "42\n42\n42\n",
    );
}

#[test]
fn archive_custom_call_sugar_preserves_defaults_names_and_variadic_packing() {
    roundtrip(
        r#"struct Defaults { value: i64 }
        impl Defaults {
            pub fn apply(self, a: i64, .b: i64 = 2) -> i64 { a + b }
            pub fn update(self, value: i64, .offset: i64 = 2) { self.value = value + offset }
        }
        struct Collector {}
        impl Collector {
            pub fn apply(self, ...values: List) -> i64 { values(0) + values(1) }
        }
        fn main() {
            let defaults = Defaults { value: 0 }
            println(defaults(40)); println(defaults(b = 2, a = 40))
            defaults() = 40
            println(defaults.value)
            defaults(offset = 1) = 41
            println(defaults.value)
            let collector = Collector {}
            println(collector(40, 2))
        }"#,
        "42\n42\n42\n42\n42\n",
    );
}

#[test]
fn archive_preserves_lexical_private_calls_in_defaults_closures_and_continuations() {
    roundtrip(
        r#"effect choose(catch k) -> i64
        struct Vault { value: i64 }
        impl Vault {
            private fn secret(self, delta: i64) -> i64 { self.value + delta }
            pub fn read(self) -> i64 { let dynamic: Any = self; dynamic.secret(2) }
            pub fn apply(self, .value: i64 = self.as(Any).secret(2)) -> i64 { value }
            pub fn reader(self) -> fn() -> i64 { let dynamic: Any = self; || dynamic.secret(2) }
            pub fn effect_read(self) -> i64 {
                let dynamic: Any = self
                let delta = choose()#
                dynamic.secret(delta)
            }
        }
        mod api {
            pub struct PackageValue { value: i64 }
            impl PackageValue { fn apply(self) -> i64 { self.value } }
        }
        struct Number { value: i64 }
        mod local {
            extend Number { pub fn apply(self, delta: i64) -> i64 { self.value + delta } }
            pub fn reader() -> fn() -> i64 { let dynamic: Any = Number { value: 40 }; || dynamic(2) }
        }
        fn main() {
            let vault = Vault { value: 40 }
            println(vault.read()); println(vault())
            let escaped = vault.reader()
            println(escaped())
            let answer = vault.effect_read()# { choose(k) => k(2) }
            println(answer)
            let package_value: Any = api.PackageValue { value: 42 }
            println(package_value())
            let extended = local.reader()
            println(extended())
        }"#,
        "42\n42\n42\n42\n42\n42\n",
    );
}

#[test]
fn archive_denies_dynamic_private_and_escaped_extension_calls() {
    for (source, expected_error) in [
        (
            "struct Vault { value: i64 }\nimpl Vault { private fn apply(self) -> i64 { self.value } }\nfn main() { let value: Any = Vault { value: 42 }; println(value()); }",
            "MethodAccessDenied",
        ),
        (
            "struct Number { value: i64 }\nmod local { extend Number { pub fn apply(self, delta: i64) -> i64 { self.value + delta } }; pub fn value() -> Any { Number { value: 40 } } }\nfn main() { let escaped = local.value(); println(escaped(2)); }",
            "MethodAccessDenied",
        ),
    ] {
        let directory = TestDirectory::new();
        let path = directory.0.join("program.ns");
        std::fs::write(&path, source).unwrap();
        assert_success(&cli("build", &path));
        let source_run = cli("run", &path);
        std::fs::remove_file(&path).unwrap();
        for output in [source_run, cli("run", &path.with_extension("nsbc"))] {
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert!(stderr.contains(expected_error), "{stderr}");
            assert!(!stderr.contains("panicked"), "{stderr}");
        }
    }
}

#[test]
fn cli_rejects_static_private_application_before_creating_an_archive() {
    let directory = TestDirectory::new();
    let path = directory.0.join("program.ns");
    std::fs::write(&path, "struct Vault { value: i64 }\nimpl Vault { private fn apply(self) -> i64 { self.value } }\nfn main() { let value = Vault { value: 42 }; println(value()); }").unwrap();
    let output = cli("build", &path);
    assert!(!output.status.success());
    assert!(!path.with_extension("nsbc").exists());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("`apply` is not visible"), "{stderr}");
}

#[test]
fn archive_preserves_disjoint_scoped_traits_and_lexical_type_queries() {
    roundtrip(
        r#"effect choose(catch k) -> i64
        struct Value {}
        trait Read { fn value(self) -> i64 }
        trait Marker {}
        mod a {
            extend Read for Value { pub fn value(self) -> i64 { 40 } }
            extend Marker for Value {}
            global saved: Read = Value {}
            struct Holder { value: Read }
            enum Envelope { wrapped(value: Read) }
            pub fn stored() -> i64 {
                let holder = Holder { value: Value {} }
                let envelope = Envelope.wrapped(Value {})
                saved.value() + holder.value.value() + (envelope match { Envelope.wrapped(value) => value.value() })
            }
            pub fn marked() -> bool { let value: Any = Value {}; let checked: Marker = value; true }
            pub fn read() -> i64 { let value: Any = Value {}; value.value() }
            pub fn reader() -> fn() -> i64 { let value: Any = Value {}; || value.value() }
            pub fn effect_read() -> i64 { let value: Any = Value {}; let delta = choose()#; value.value() + delta }
            pub fn typed() -> i64 { let value: Any = Value {}; let checked: Read = value; checked.value() }
        }
        mod b {
            extend Read for Value { pub fn value(self) -> i64 { 2 } }
            pub fn read() -> i64 { Value {}.value() }
            pub fn typed() -> i64 { let value: Any = Value {}; value.as(Read).value() }
        }
        fn main() {
            println(a.read()); println(b.read())
            let escaped = a.reader()
            println(escaped())
            let answer = a.effect_read()# { choose(k) => k(2) }
            println(answer)
            println(a.typed()); println(b.typed())
            println(a.marked()); println(a.stored())
        }"#,
        "40\n2\n40\n42\n40\n2\ntrue\n120\n",
    );
}

#[test]
fn archive_rejects_scoped_trait_methods_and_casts_outside_their_scopes() {
    for expression in ["value.value()", "value.as(Read).value()"] {
        let directory = TestDirectory::new();
        let path = directory.0.join("program.ns");
        let source = format!(
            "struct Value {{}}\ntrait Read {{fn value(self)->i64}}\nmod local {{extend Read for Value {{pub fn value(self)->i64{{42}}}}}}\nfn main(){{let value:Any=Value{{}};println({expression});}}"
        );
        std::fs::write(&path, source).unwrap();
        assert_success(&cli("build", &path));
        let source_run = cli("run", &path);
        std::fs::remove_file(&path).unwrap();
        for output in [source_run, cli("run", &path.with_extension("nsbc"))] {
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert!(
                stderr.contains(if expression.contains(".as") {
                    "TypeError"
                } else {
                    "MethodAccessDenied"
                }),
                "{stderr}"
            );
        }
    }
}

#[test]
fn cli_rejects_duplicate_scoped_trait_identity_before_writing_an_archive() {
    let directory = TestDirectory::new();
    let path = directory.0.join("program.ns");
    std::fs::write(&path, "struct Value{}\ntrait Empty{}\nmod local{extend Empty for Value{};extend Empty for Value{}}\nfn main(){42}").unwrap();
    let output = cli("build", &path);
    assert!(!output.status.success());
    assert!(!path.with_extension("nsbc").exists());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("duplicate trait implementation"),
        "{stderr}"
    );
}

#[test]
fn archive_preserves_parent_first_trait_dispatch_schemas_and_aliases() {
    let source = r#"struct Value {}
        trait SchemaParent { fn first(self) -> i64 }
        trait SchemaChild(SchemaParent) { fn second(self) -> i64 }
        typealias SchemaAlias = SchemaChild
        impl SchemaParent for Value { pub fn first(self) -> i64 { 40 } }
        impl SchemaChild for Value { pub fn second(self) -> i64 { 2 } }
        fn read(value: SchemaAlias) -> i64 { value.first() + value.second() }
        fn main() { println(read(Value {})) }"#;
    let directory = TestDirectory::new();
    let path = directory.0.join("schema.ns");
    std::fs::write(&path, source).unwrap();
    assert_success(&cli("build", &path));
    let archive_path = path.with_extension("nsbc");
    std::fs::remove_file(path).unwrap();
    let bytes = std::fs::read(&archive_path).unwrap();
    let artifact = nsbc_io::read_artifact(&bytes).unwrap();
    let snapshot = artifact.type_pool.snapshot();
    let child = snapshot
        .trait_schemas
        .iter()
        .find(|schema| {
            artifact
                .type_pool
                .display_name(schema.trait_type)
                .as_deref()
                == Some("SchemaChild")
        })
        .unwrap();
    let names: Vec<_> = child
        .slots
        .iter()
        .map(|key| str_interner::get(key.name))
        .collect();
    assert_eq!(names, ["first", "second"]);
    assert_eq!(
        artifact
            .type_pool
            .display_name(child.slots[0].trait_owner)
            .as_deref(),
        Some("SchemaParent")
    );
    assert_eq!(child.slots[1].trait_owner, child.trait_type);
    let table = artifact
        .type_pool
        .vtables_snapshot()
        .iter()
        .find(|table| table.trait_type == child.trait_type)
        .unwrap();
    assert_eq!(table.entries.len(), 2);
    assert_ne!(table.entries[0], table.entries[1]);
    let resaved = nsbc_io::write_artifact(&artifact).unwrap();
    let restored = nsbc_io::read_artifact(&resaved).unwrap();
    assert_eq!(
        restored.type_pool.snapshot().trait_schemas,
        snapshot.trait_schemas
    );
    std::fs::write(&archive_path, resaved).unwrap();
    let execution = cli("run", &archive_path);
    assert_success(&execution);
    assert_eq!(String::from_utf8(execution.stdout).unwrap(), "42\n");
}

#[test]
fn archive_preserves_nested_self_signature_and_explicit_trait_annotation() {
    roundtrip(
        r#"struct Value {}
        typealias Alias = Value
        trait Pack {
            fn pack(self, value: (Self, i64)) -> (Self, i64)
            fn accept(self, value: Pack) -> i64
        }
        impl Pack for Alias {
            pub fn pack(self, value: (Alias, i64)) -> (Alias, i64) { value }
            pub fn accept(self, value: Pack) -> i64 { 42 }
        }
        fn main() {
            let value = Value {}
            println(value.pack((value, 42)).1)
            println(value.accept(value))
        }"#,
        "42\n42\n",
    );
}

#[test]
fn invalid_trait_signatures_do_not_produce_archives() {
    for source in [
        "struct Value {}\ntrait Read { fn value(self) -> i64 }\nimpl Read for Value { pub fn value(self) -> bool { true } }\nfn main() {}",
        "struct Value {}\ntrait Add { fn add(self, value: i64) -> i64 }\nimpl Add for Value { pub fn add(self, value: bool) -> i64 { 42 } }\nfn main() {}",
        "struct Value {}\ntrait Add { fn add(self, value: i64) -> i64 }\nimpl Add for Value { pub fn add(self) -> i64 { 42 } }\nfn main() {}",
        "struct Value {}\ntrait Share { fn share(self, value: Share) -> i64 }\nimpl Share for Value { pub fn share(self, value: Value) -> i64 { 42 } }\nfn main() {}",
    ] {
        let directory = TestDirectory::new();
        let path = directory.0.join("bad-signature.ns");
        std::fs::write(&path, source).unwrap();
        let result = cli("build", &path);
        assert!(
            !result.status.success(),
            "accepted invalid trait signature: {source}"
        );
        let error = String::from_utf8_lossy(&result.stderr);
        assert!(
            error.contains("signature") || error.contains("contract"),
            "{error}"
        );
        assert!(!path.with_extension("nsbc").exists());
    }
}

#[test]
fn artifact_writer_rejects_replaced_trait_target_signature() {
    let driver = driver::Driver::new();
    let result = driver.compile(
        "struct Value {}\ntrait Checked { fn interface_checked_read(self) -> i64 }\nimpl Checked for Value { pub fn interface_checked_read(self) -> i64 { 42 } }\nimpl Value { pub fn interface_bad_return(self) -> bool { true } }\nfn main() { println(Value {}.interface_checked_read()) }",
    );
    assert!(!result.has_errors);
    let mut artifact = result.into_artifact().unwrap();
    let schema = artifact
        .type_pool
        .trait_schemas_snapshot()
        .iter()
        .find(|schema| {
            schema
                .slots
                .iter()
                .any(|key| str_interner::get(key.name) == "interface_checked_read")
        })
        .unwrap();
    let table = artifact
        .type_pool
        .vtables_snapshot()
        .iter()
        .find(|table| table.trait_type == schema.trait_type)
        .unwrap();
    let target = table.entries[0];
    let replacement = artifact
        .type_pool
        .methods_of(table.implementor)
        .iter()
        .find(|method| str_interner::get(method.name) == "interface_bad_return")
        .unwrap()
        .func_id;
    let wrong_type = artifact
        .codegen_output
        .functions
        .iter()
        .find(|function| function.func_id.0 == replacement)
        .unwrap()
        .function_type;
    artifact
        .codegen_output
        .functions
        .iter_mut()
        .find(|function| function.func_id.0 == target)
        .unwrap()
        .function_type = wrong_type;
    let error = nsbc_io::write_artifact(&artifact).unwrap_err().to_string();
    assert!(
        error.contains("signature") || error.contains("contract"),
        "{error}"
    );
}

#[test]
fn archive_executes_escaping_closure_with_explicit_function_abi() {
    let directory = TestDirectory::new();
    let path = directory.0.join("closure-abi.ns");
    std::fs::write(&path,
        "fn make(value:i64)->fn()->i64 { || value }\nfn main(){let callback=make(42);println(callback())}",
    ).unwrap();
    assert_success(&cli("build", &path));
    let archive_path = path.with_extension("nsbc");
    std::fs::remove_file(path).unwrap();
    let artifact = nsbc_io::read_artifact(&std::fs::read(&archive_path).unwrap()).unwrap();
    assert!(
        artifact
            .codegen_output
            .functions
            .iter()
            .all(|function| function.abi.is_some())
    );
    let entry = artifact
        .codegen_output
        .functions
        .iter()
        .find(|function| Some(function.func_id) == artifact.entry)
        .unwrap();
    let entry_abi = entry.abi.as_ref().unwrap();
    assert!(entry_abi.captures.is_empty());
    assert!(entry_abi.parameters.is_empty());
    let captured: Vec<_> = artifact
        .codegen_output
        .functions
        .iter()
        .filter(|function| {
            function
                .abi
                .as_ref()
                .is_some_and(|abi| !abi.captures.is_empty())
        })
        .collect();
    assert_eq!(captured.len(), 1);
    assert!(captured[0].is_closure);
    let closure_abi = captured[0].abi.as_ref().unwrap();
    assert_eq!(closure_abi.captures.len(), 1);
    assert!(closure_abi.parameters.is_empty());
    let output = cli("run", &archive_path);
    assert_success(&output);
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "42\n");
}

#[test]
fn archive_transports_selected_scoped_trait_proofs_into_external_functions() {
    roundtrip(
        r#"struct Value {}
        trait Read { fn value(self) -> i64 }
        fn consume(value: Read) -> i64 { value.value() }
        mod a {
            extend Read for Value { pub fn value(self) -> i64 { 40 } }
            pub fn answer() -> i64 { consume(Value {}) }
        }
        mod b {
            extend Read for Value { pub fn value(self) -> i64 { 2 } }
            pub fn answer() -> i64 { consume(Value {}) }
        }
        fn main() { println(a.answer() + b.answer()) }"#,
        "42\n",
    );
}

#[test]
fn archive_trait_slots_disambiguate_same_names_and_project_parent_interfaces() {
    roundtrip(
        r#"struct Value {}
        trait Left { fn value(self) -> i64 }
        trait Right { fn value(self) -> i64 }
        trait Parent { fn first(self) -> i64 }
        trait Child(Parent) { fn second(self) -> i64 }
        impl Left for Value { pub fn value(self) -> i64 { 40 } }
        impl Right for Value { pub fn value(self) -> i64 { 2 } }
        impl Parent for Value { pub fn first(self) -> i64 { 40 } }
        impl Child for Value { pub fn second(self) -> i64 { 2 } }
        fn left(value: Left) -> i64 { value.value() }
        fn right(value: Right) -> i64 { value.value() }
        fn parent(value: Parent) -> i64 { value.first() }
        fn child(value: Child) -> i64 { parent(value) + value.second() }
        fn main() {
            println(left(Value {})); println(right(Value {})); println(child(Value {}))
        }"#,
        "40\n2\n42\n",
    );
}

#[test]
fn archive_trait_proofs_survive_heap_receiver_closures_and_repeated_resumes() {
    roundtrip(
        r#"effect choose(catch k) -> i64
        struct Value { number: i64, text: String }
        trait Read { fn value(self) -> i64 }
        fn capture(value: Read) -> fn() -> i64 { || value.value() }
        fn delayed(value: Read) -> i64 { let extra = choose()#; value.value() + extra }
        mod a {
            extend Read for Value { pub fn value(self) -> i64 { self.number + self.text.len() } }
            pub fn saved() -> fn() -> i64 { capture(Value { number: 38, text: "hi" }) }
            pub fn repeated() -> i64 {
                delayed(Value { number: 38, text: "hi" })# { choose(k) => k(1) + k(2) }
            }
        }
        fn main() { let callback = a.saved(); println(callback()); println(a.repeated()) }"#,
        "40\n83\n",
    );
}

#[test]
fn archive_dispatches_bootstrap_trait_proofs_to_checked_user_methods() {
    roundtrip(
        r#"struct Value { number: i64 }
        impl Eq for Value { pub fn eq(self, other: Self) -> bool { self.number == other.number } }
        impl PartialEq for Value { pub fn eq(self, other: Self) -> bool { self.number == other.number + 1 } }
        trait Child(Eq) {}
        impl Child for Value {}
        impl Display for Value { pub fn to_string(self) -> String { "checked" } }
        fn equal(left: Eq, right: Eq) -> bool { left.eq(right) }
        fn partial(left: PartialEq, right: PartialEq) -> bool { left.eq(right) }
        fn child(left: Child, right: Child) -> bool { left.eq(right) }
        fn display(value: Display) -> String { value.to_string() }
        fn main() {
            println(equal(Value { number: 42 }, Value { number: 42 }))
            println(partial(Value { number: 42 }, Value { number: 41 }))
            println(display(Value { number: 42 }))
            println(child(Value { number: 42 }, Value { number: 42 }))
        }"#,
        "true\ntrue\nchecked\ntrue\n",
    );
}

#[test]
fn invalid_bootstrap_trait_contracts_do_not_produce_archives() {
    for source in [
        "struct Value{}\nimpl Eq for Value{pub fn eq(self,other:bool)->bool{true}}\nfn main(){}",
        "struct Value{}\nimpl PartialEq for Value{pub fn eq(self,other:Self)->i64{42}}\nfn main(){}",
        "struct Value{}\nimpl Display for Value{pub fn to_string(self,extra:i64)->String{\"bad\"}}\nfn main(){}",
        "struct Value{}\nimpl Display for Value{pub fn to_string(self)->bool{true}}\nfn main(){}",
        "struct Value{}\nimpl Iterator for Value{pub fn has_next(self)->i64{42};pub fn next(self)->i64{42}}\nfn main(){}",
    ] {
        let directory = TestDirectory::new();
        let path = directory.0.join("bad-bootstrap.ns");
        std::fs::write(&path, source).unwrap();
        let result = cli("build", &path);
        assert!(
            !result.status.success(),
            "accepted invalid bootstrap contract: {source}"
        );
        let error = String::from_utf8_lossy(&result.stderr);
        assert!(
            error.contains("signature") || error.contains("require"),
            "{error}"
        );
        assert!(!path.with_extension("nsbc").exists());
    }
}

#[test]
fn archive_default_trait_adapters_keep_concrete_and_declaring_trait_identities() {
    roundtrip(
        r#"
        struct Forty {};
        struct Two {};
        struct Override {};
        trait Sum { fn value(self)->i64; derive fn sum(self)->i64 { self.value() } };
        impl Sum for Forty { pub fn value(self)->i64 {40} };
        impl Sum for Two { pub fn value(self)->i64 {2} };
        impl Sum for Override { pub fn value(self)->i64 {0}; pub fn sum(self)->i64 {42} };
        fn consume(x:Sum)->i64 {x.sum()};
        struct Both {};
        trait Left { fn value(self)->i64; derive fn summary(self)->i64 {self.value()} };
        trait Right { fn value(self)->i64; derive fn summary(self)->i64 {self.value()} };
        impl Left for Both {pub fn value(self)->i64 {40}};
        impl Right for Both {pub fn value(self)->i64 {2}};
        fn left(x:Left)->i64 {x.summary()};
        fn right(x:Right)->i64 {x.summary()};
        struct Boxed {number:i64};
        trait Identity {derive fn identity(self)->Self {self}};
        impl Identity for Boxed {};
        fn main() {
            println(consume(Forty{}) + consume(Two{}));
            println(consume(Override{}));
            println(left(Both{}) + right(Both{}));
            let value:Boxed = Boxed{number:42}.identity();
            println(value.number)
        }
        "#,
        "42\n42\n42\n42\n",
    );
}

#[test]
fn archive_default_trait_adapters_preserve_inherited_and_scoped_proofs() {
    roundtrip(
        r#"
        struct P {};
        trait Base {fn value(self)->i64; derive fn sum(self)->i64 {self.value()+2}};
        trait Child(Base) {};
        impl Base for P {pub fn value(self)->i64 {2}};
        impl Child for P {pub fn value(self)->i64 {40}};
        fn base(x:Base)->i64 {x.sum()};
        fn child(x:Child)->i64 {base(x)};
        struct Scoped {};
        trait Read {fn value(self)->i64; derive fn sum(self,other:Self)->i64 {self.value()+other.value()}};
        mod a {
            extend Read for Scoped {pub fn value(self)->i64 {40}};
            pub fn answer()->i64 {outside.start(Scoped{})}
        };
        mod outside {
            extend Read for Scoped {pub fn value(self)->i64 {2}};
            pub fn start(x:Read)->i64 {x.sum(Scoped{})}
        };
        fn main() {println(child(P{})); println(base(P{})); println(a.answer())}
        "#,
        "42\n4\n42\n",
    );
}

#[test]
fn archive_default_trait_adapters_preserve_nested_closures_and_multishot_effects() {
    roundtrip(
        r#"
        effect pause(catch k)->i64;
        struct P {text:String};
        trait Read {
            fn value(self)->i64;
            derive fn increment(self)->i64 {self.value()+1};
            derive fn sum(self)->i64 {let f=||self.increment(); let n=pause()#; f()+n}
        };
        mod a {
            extend Read for P {pub fn value(self)->i64 {self.text.len()}};
            pub fn answer()->i64 {
                let saved=outside.consume(P{text:"abcd"++"efgh"})# {pause(k)=>k};
                let first=saved(33);
                let second=saved(34);
                if first==42 and second==43 {42} else {0}
            }
        };
        mod outside {pub fn consume(x:Read)->i64 {x.sum()#}};
        fn main() {println(a.answer())}
        "#,
        "42\n",
    );
}

#[test]
fn archive_executes_generated_display_and_comparison_methods_through_trait_proofs() {
    roundtrip(
        r#"
        struct Displayed {number:i64, text:String};
        derive Display for Displayed;
        struct Equal {number:i64};
        derive Eq for Equal;
        struct Partial {number:i64};
        derive PartialEq for Partial;
        fn display(value:Display)->String {value.to_string()};
        fn equal(left:Eq,right:Eq)->bool {left.eq(right)};
        fn partial(left:PartialEq,right:PartialEq)->bool {left.eq(right)};
        fn main() {
            println(display(Displayed{number:42,text:"你好"}));
            println(equal(Equal{number:42},Equal{number:42}));
            println(equal(Equal{number:42},Equal{number:2}));
            println(partial(Partial{number:42},Partial{number:42}));
            println(partial(Partial{number:42},Partial{number:2}))
        }
        "#,
        "Displayed { number: 42, text: 你好 }\ntrue\nfalse\ntrue\nfalse\n",
    );
}

#[test]
fn generated_display_archive_rejects_missing_and_incompatible_native_imports() {
    let compiled = driver::Driver::new().compile(
        "struct Value{number:i64}\nderive Display for Value\nfn display(x:Display)->String{x.to_string()}\nfn main(){println(display(Value{number:42}))}",
    );
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let artifact = compiled.into_artifact().unwrap();
    let native = artifact
        .builtins
        .iter()
        .find(|import| import.name == "__derived_display")
        .expect("generated Display must declare its new native capability")
        .clone();
    let bytes = nsbc_io::write_artifact(&artifact).unwrap();
    let mut missing = nsbc_io::read_artifact(&bytes).unwrap();
    missing.builtins.retain(|import| import.id != native.id);
    let error = nsbc_io::write_artifact(&missing).unwrap_err().to_string();
    assert!(error.contains("manifest"), "{error}");

    for wrong_name in [false, true] {
        let mut bad = nsbc_io::read_artifact(&bytes).unwrap();
        if wrong_name {
            bad.builtins
                .iter_mut()
                .find(|import| import.id == native.id)
                .unwrap()
                .name = "forged_native_display".into();
        } else {
            bad.builtin_abi_version = 3;
        }
        let directory = TestDirectory::new();
        let path = directory.0.join("bad-display.nsbc");
        std::fs::write(&path, nsbc_io::write_artifact(&bad).unwrap()).unwrap();
        let output = cli("run", &path);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("incompatible builtin ABI"), "{error}");
    }
}

#[test]
fn archive_executes_intrinsic_trait_wrappers_with_exact_concrete_signatures() {
    roundtrip(
        r#"
        fn equal(left:Eq,right:Eq)->bool {left.eq(right)};
        fn partial(left:PartialEq,right:PartialEq)->bool {left.eq(right)};
        fn display(value:Display)->String {value.to_string()};
        fn main() {
            println(equal(42,42)); println(equal(40,2));
            println(equal(true,true)); println(equal("你好","你好"));
            println(partial(2.5,2.5)); println(partial(2.5,1.5));
            let wide:i128=170141183460469231731687303715884105727;
            println(equal(wide,wide));
            println(display(42)); println(display(true)); println(display("你好"));
            println(display(wide))
        }
        "#,
        "true\nfalse\ntrue\ntrue\ntrue\nfalse\ntrue\n42\ntrue\n你好\n170141183460469231731687303715884105727\n",
    );
}

#[test]
fn intrinsic_trait_comparison_rejects_different_concrete_implementations() {
    for source in [
        "fn equal(a:Eq,b:Eq)->bool{a.eq(b)}\nfn main(){println(equal(42,true))}",
        "fn equal(a:PartialEq,b:PartialEq)->bool{a.eq(b)}\nfn main(){println(equal(42,42.0))}",
    ] {
        let directory = TestDirectory::new();
        let path = directory.0.join("different-proof-data.ns");
        std::fs::write(&path, source).unwrap();
        assert_success(&cli("build", &path));
        std::fs::remove_file(&path).unwrap();
        let output = cli("run", &path.with_extension("nsbc"));
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("InvalidTraitProof"), "{error}");
    }
}

#[test]
fn intrinsic_trait_archive_preserves_standard_library_function_contracts() {
    let compiled = driver::Driver::new().compile(
        "fn equal(a:Eq,b:Eq)->bool{a.eq(b)}\nfn display(x:Display)->String{x.to_string()}\nfn main(){println(equal(42,42));println(display(true))}",
    );
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let artifact = compiled.into_artifact().unwrap();
    let restored = nsbc_io::read_artifact(&nsbc_io::write_artifact(&artifact).unwrap()).unwrap();
    assert_eq!(
        restored.type_pool.vtables_snapshot().len(),
        artifact.type_pool.vtables_snapshot().len()
    );
    let mut checked = 0;
    for (original, loaded) in artifact
        .type_pool
        .vtables_snapshot()
        .iter()
        .zip(restored.type_pool.vtables_snapshot())
    {
        assert_eq!(loaded.trait_type, original.trait_type);
        assert_eq!(loaded.implementor, original.implementor);
        assert_eq!(loaded.visible_scope, original.visible_scope);
        assert_eq!(loaded.entries, original.entries);
        let schema = restored.type_pool.trait_schema(loaded.trait_type).unwrap();
        for (key, &target) in schema.slots.iter().zip(&loaded.entries) {
            let function = restored
                .codegen_output
                .functions
                .iter()
                .find(|function| function.func_id.0 == target)
                .unwrap();
            restored
                .type_pool
                .check_trait_method_signature(key, loaded.implementor, function.function_type)
                .unwrap();
            let original = artifact
                .codegen_output
                .functions
                .iter()
                .find(|function| function.func_id.0 == target)
                .unwrap();
            assert_eq!(function.function_type, original.function_type);
            assert_eq!(function.abi, original.abi);
            checked += 1;
        }
    }
    assert!(checked >= 3, "primitive contracts must be present");
    assert!(
        restored
            .builtins
            .iter()
            .any(|import| import.name == "__scalar_eq"),
        "scalar comparison capability must be declared"
    );
    let mut stale = restored;
    stale.builtin_abi_version = 4;
    let directory = TestDirectory::new();
    let path = directory.0.join("stale-scalar-abi.nsbc");
    std::fs::write(&path, nsbc_io::write_artifact(&stale).unwrap()).unwrap();
    let output = cli("run", &path);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("incompatible builtin ABI"));
}

#[test]
fn archive_preserves_character_unicode_and_escape_literals() {
    roundtrip(
        r#"fn main(){println('你');println('🦀');println('\n'=='\n');println('\t'=='\t');println('\''=='\'');println('\\'=='\\')}"#,
        "你\n🦀\ntrue\ntrue\ntrue\ntrue\n",
    );
}

#[test]
fn archive_preserves_inferred_and_named_default_method_self_specialization() {
    let source = r#"
        struct P {value:i64};
        struct Q {value:i64};
        trait Read {
            derive fn callback(self)->fn()->Self {||self};
            derive fn named(self)->fn(Self)->Self {
                fn identity(other:Self)->Self {other};
                identity
            };
            derive fn own_type(self)->Type {
                fn own()->Type {Self};
                own()
            };
            derive fn explicit(self)->fn(Read)->Read {
                fn identity(other:Read)->Read {other};
                identity
            }
        };
        impl Read for P {};
        impl Read for Q {};
        fn main() {
            let p=P{value:40}; let q=Q{value:2};
            let pc=p.callback(); let qc=q.callback();
            println(pc'type==fn()->P); println(qc'type==fn()->Q);
            println(pc().value+qc().value);
            let pn=p.named(); let qn=q.named();
            println(pn'type==fn(P)->P); println(qn'type==fn(Q)->Q);
            println(pn(p).value+qn(q).value);
            println(p.own_type()==P); println(q.own_type()==Q);
            println(p.explicit()'type==fn(Read)->Read);
            println(q.explicit()'type==fn(Read)->Read)
        }
        "#;
    roundtrip(
        source,
        "true\ntrue\n42\ntrue\ntrue\n42\ntrue\ntrue\ntrue\ntrue\n",
    );
    let compiled = driver::Driver::new().compile(source);
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let artifact = compiled.into_artifact().unwrap();
    let restored = nsbc_io::read_artifact(&nsbc_io::write_artifact(&artifact).unwrap()).unwrap();
    assert_eq!(
        restored.codegen_output.functions.len(),
        artifact.codegen_output.functions.len()
    );
    for original in &artifact.codegen_output.functions {
        let function = restored
            .codegen_output
            .functions
            .iter()
            .find(|function| function.func_id == original.func_id)
            .unwrap();
        assert_eq!(function.function_type, original.function_type);
        assert_eq!(function.abi, original.abi);
        assert_eq!(
            function.abi.as_ref().unwrap().physical_parameter_count(),
            function.param_count as usize
        );
    }
}

#[test]
fn default_self_callback_rejects_explicit_trait_values_with_wrong_concrete_type() {
    let directory = TestDirectory::new();
    let path = directory.0.join("bad-self-callback.ns");
    std::fs::write(
        &path,
        "struct P{};struct Q{};trait Read{derive fn callback(self,other:Read)->fn()->Self{||other}};impl Read for P{};impl Read for Q{};fn main(){let f=P{}.callback(Q{});f();42}",
    )
    .unwrap();
    let source_run = cli("run", &path);
    assert!(!source_run.status.success());
    assert!(source_run.stdout.is_empty());
    assert!(String::from_utf8_lossy(&source_run.stderr).contains("TypeError"));
    assert_success(&cli("build", &path));
    std::fs::remove_file(&path).unwrap();
    let archive_run = cli("run", &path.with_extension("nsbc"));
    assert!(!archive_run.status.success());
    assert!(archive_run.stdout.is_empty());
    assert!(String::from_utf8_lossy(&archive_run.stderr).contains("TypeError"));
}

fn legacy_archive_fixture(
    mut pool: type_pool::TypePool,
    instructions: Vec<nsbc::Instruction>,
    constants: Vec<nsbc::Constant>,
    builtins: Vec<nsbc::BuiltinImport>,
    builtin_abi_version: u32,
) -> nsbc::CompiledArtifact {
    let function_type = pool.intern_structural(type_pool::TypeKind::Function {
        params: vec![],
        ret: type_pool::Intrinsic::Unit.type_index(),
    });
    nsbc::CompiledArtifact {
        codegen_output: nsbc::CodegenOutput {
            method_call_scopes: None,
            scope_coverage: nsbc::ScopeCoverage::Calls,
            functions: vec![nsbc::CompiledFunction {
                display_owner: None,
                func_id: nsbc::FuncId(0),
                name: str_interner::intern("legacy_entry"),
                instructions: instructions
                    .into_iter()
                    .map(nsbc::Instruction::encode)
                    .collect(),
                register_count: 5,
                param_count: 0,
                is_closure: false,
                function_type,
                abi: None,
                safepoint_pcs: vec![],
            }],
            constants,
            globals: vec![],
        },
        type_pool: pool,
        entry: Some(nsbc::FuncId(0)),
        builtin_abi_version,
        builtins,
    }
}

#[test]
fn legacy_integer_ordering_archive_keeps_its_original_protocol() {
    use nsbc::{Instruction, Opcode, Reg};
    use type_pool::{Intrinsic, TypeId, TypeInfo, TypeKind, TypePool};

    let mut pool = TypePool::with_intrinsics();
    let owner = pool.register(TypeInfo {
        kind: TypeKind::Struct {
            name: str_interner::intern("LegacyOrdered"),
            fields: vec![type_pool::FieldInfo {
                name: str_interner::intern("number"),
                ty: Intrinsic::I64.type_index(),
                offset: 0,
                has_default: false,
            }],
        },
        type_id: TypeId(905, 1),
        size: 8,
        align: 8,
    });
    let view = pool.well_known.ord;
    let name = str_interner::intern("cmp");
    let method = type_pool::MethodSlot {
        name,
        func_id: type_pool::DERIVE_FUNC_ID,
        access: type_pool::MethodAccess::Public,
        trait_impl: Some(view),
        visible_scope: None,
    };
    pool.add_method(owner, method.clone());
    pool.add_trait_impl(type_pool::TraitImplRecord {
        visible_scope: None,
        trait_type: view,
        implementor: owner,
        methods: vec![method],
    });
    pool.add_vtable(type_pool::VTable {
        visible_scope: None,
        trait_type: view,
        implementor: owner,
        entries: vec![type_pool::DERIVE_FUNC_ID],
    });
    let artifact = legacy_archive_fixture(
        pool,
        vec![
            Instruction::new_object(Reg(2), owner),
            Instruction::load_const(Reg(0), 0),
            Instruction::store_field(Reg(2), 0, Reg(0)),
            Instruction::new_object(Reg(3), owner),
            Instruction::load_const(Reg(0), 1),
            Instruction::store_field(Reg(3), 0, Reg(0)),
            Instruction::mov(Reg(0), Reg(3)),
            Instruction::call_method(Reg(2), name.as_u32(), 1),
            Instruction::mov(Reg(4), Reg(0)),
            Instruction::call_builtin(1, 1),
            Instruction::load_imm(Reg(1), 0),
            Instruction::r_type(Opcode::CmpGt, Reg(0), Reg(4), Reg(1)),
            Instruction::call_builtin(1, 1),
            Instruction::return_unit(),
        ],
        vec![
            nsbc::Constant::Int(i64::MAX),
            nsbc::Constant::Int(i64::MAX / 2),
        ],
        vec![nsbc::BuiltinImport {
            id: 1,
            name: "println".into(),
        }],
        1,
    );
    let bytes = nsbc_io::write_artifact(&artifact).unwrap();
    let restored = nsbc_io::read_artifact(&bytes).unwrap();
    assert!(restored.type_pool.trait_schema(view).is_none());
    let TypeKind::Trait { parents, .. } = &restored.type_pool.get(view).kind else {
        panic!("Ord must remain a trait")
    };
    assert!(parents.is_empty());
    assert!(restored.type_pool.snapshot().types.iter().all(|info| {
        !matches!(&info.kind, TypeKind::Enum{name,..} if str_interner::get(*name)=="Ordering")
    }));
    assert_eq!(
        restored.type_pool.find_method(owner, name).unwrap().func_id,
        type_pool::DERIVE_FUNC_ID
    );
    assert!(restored.codegen_output.functions[0].abi.is_none());
    let directory = TestDirectory::new();
    let path = directory.0.join("legacy-cmp.nsbc");
    std::fs::write(&path, bytes).unwrap();
    let output = cli("run", &path);
    assert_success(&output);
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "1\ntrue\n");
}

#[test]
fn legacy_scalar_equality_abi_five_remains_executable() {
    use nsbc::{Instruction, Reg};
    let artifact = legacy_archive_fixture(
        type_pool::TypePool::with_intrinsics(),
        vec![
            Instruction::load_imm(Reg(0), 42),
            Instruction::load_imm(Reg(1), 42),
            Instruction::call_builtin(121, 2),
            Instruction::call_builtin(1, 1),
            Instruction::return_unit(),
        ],
        vec![],
        vec![
            nsbc::BuiltinImport {
                id: 121,
                name: "__scalar_eq".into(),
            },
            nsbc::BuiltinImport {
                id: 1,
                name: "println".into(),
            },
        ],
        5,
    );
    let directory = TestDirectory::new();
    let path = directory.0.join("legacy-scalar-eq.nsbc");
    std::fs::write(&path, nsbc_io::write_artifact(&artifact).unwrap()).unwrap();
    let output = cli("run", &path);
    assert_success(&output);
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "true\n");
}

#[test]
fn archive_executes_primitive_ordering_traits_and_unordered_relations() {
    roundtrip(
        r#"
        fn total(a:Ord,b:Ord)->Ordering {a.cmp(b)};
        fn partial(a:PartialOrd,b:PartialOrd)->?Ordering {a.partial_cmp(b)};
        fn main() {
            println(total(40,42)==Ordering.less);
            println(total(true,false)==Ordering.greater);
            println(total('你','你')==Ordering.equal);
            println(total("a","b")==Ordering.less);
            let wide:i128=170141183460469231731687303715884105727;
            println(total(wide,wide-1)==Ordering.greater);
            println(partial(2.5,1.5)==Ordering.greater);
            let nan:f64=sqrt(-1.0);
            println(partial(nan,nan)==null);
            println(not(nan<nan) and not(nan<=nan) and not(nan>nan) and not(nan>=nan));
            println(Ordering.less<Ordering.equal and Ordering.equal<Ordering.greater)
        }
        "#,
        "true\ntrue\ntrue\ntrue\ntrue\ntrue\ntrue\ntrue\ntrue\n",
    );
}

#[test]
fn archive_executes_custom_and_derived_struct_enum_tuple_ordering() {
    roundtrip(
        r#"
        global calls:i64=0;
        struct Key {number:i64};
        derive Eq for Key;
        impl Ord for Key {pub fn cmp(self,other:Self)->Ordering {calls+=1; if self.number<other.number {Ordering.less} else if self.number>other.number {Ordering.greater} else {Ordering.equal}}};
        struct Pair {first:Key,last:i64};
        derive Eq,Ord for Pair;
        enum Entry {none,item(key:Key,number:i64)};
        derive Eq,Ord for Entry;
        typealias Row=(i64,String);
        derive Eq,Ord for Row;
        struct Maybe {number:?i64};
        derive Eq,Ord for Maybe;
        struct Floating {number:f64};
        derive PartialEq,PartialOrd for Floating;
        fn total(a:Ord,b:Ord)->Ordering {a.cmp(b)};
        fn partial(a:PartialOrd,b:PartialOrd)->?Ordering {a.partial_cmp(b)};
        fn main() {
            println(total(Pair{first:Key{number:1},last:100},Pair{first:Key{number:2},last:0})==Ordering.less);
            println(calls);
            println(total(Entry.none,Entry.item(Key{number:0},0))==Ordering.less);
            println(total(Entry.item(Key{number:42},1),Entry.item(Key{number:42},2))==Ordering.less);
            let a:Row=(42,"a");let b:Row=(42,"b");
            println(total(a,b)==Ordering.less);
            println(total(Maybe{number:null},Maybe{number:0})==Ordering.less);
            let nan:f64=sqrt(-1.0);let x=Floating{number:nan};let y=Floating{number:42.0};
            println(partial(x,y)==null);
            println(not(x<y) and not(x<=y) and not(x>y) and not(x>=y))
        }
        "#,
        "true\n1\ntrue\ntrue\ntrue\ntrue\ntrue\ntrue\n",
    );
}

#[test]
fn ordering_trait_archive_rejects_mixed_concrete_receiver_proofs() {
    for source in [
        "fn compare(a:Ord,b:Ord)->Ordering{a.cmp(b)}\nfn main(){println(compare(42,true))}",
        "fn compare(a:PartialOrd,b:PartialOrd)->?Ordering{a.partial_cmp(b)}\nfn main(){println(compare(42,42.0))}",
    ] {
        let directory = TestDirectory::new();
        let path = directory.0.join("mixed-ordering.ns");
        std::fs::write(&path, source).unwrap();
        assert_success(&cli("build", &path));
        std::fs::remove_file(&path).unwrap();
        let output = cli("run", &path.with_extension("nsbc"));
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("InvalidTraitProof"));
    }
}

#[test]
fn scalar_ordering_archive_rejects_stale_abi_and_untrusted_native_manifest() {
    use nsbc::{Instruction, Reg};
    let artifact = legacy_archive_fixture(
        type_pool::TypePool::with_intrinsics(),
        vec![
            Instruction::load_imm(Reg(0), 42),
            Instruction::load_imm(Reg(1), 41),
            Instruction::call_builtin(122, 2),
            Instruction::call_builtin(1, 1),
            Instruction::return_unit(),
        ],
        vec![],
        vec![
            nsbc::BuiltinImport {
                id: 122,
                name: "__scalar_cmp".into(),
            },
            nsbc::BuiltinImport {
                id: 1,
                name: "println".into(),
            },
        ],
        6,
    );
    let bytes = nsbc_io::write_artifact(&artifact).unwrap();
    let mut missing = nsbc_io::read_artifact(&bytes).unwrap();
    missing.builtins.retain(|import| import.id != 122);
    assert!(
        nsbc_io::write_artifact(&missing)
            .unwrap_err()
            .to_string()
            .contains("manifest")
    );
    for wrong_name in [false, true] {
        let mut bad = nsbc_io::read_artifact(&bytes).unwrap();
        if wrong_name {
            bad.builtins
                .iter_mut()
                .find(|import| import.id == 122)
                .unwrap()
                .name = "forged_scalar_cmp".into();
        } else {
            bad.builtin_abi_version = 5;
        }
        let directory = TestDirectory::new();
        let path = directory.0.join("bad-scalar-cmp.nsbc");
        std::fs::write(&path, nsbc_io::write_artifact(&bad).unwrap()).unwrap();
        let output = cli("run", &path);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("incompatible builtin ABI"));
    }
}

#[test]
fn ordering_archive_preserves_parent_slots_result_identity_and_function_abi() {
    use type_pool::TypeKind;
    let compiled=driver::Driver::new().compile(
        "struct P{number:i64};derive Eq,PartialEq,Ord,PartialOrd for P;fn total(a:Ord,b:Ord)->Ordering{a.cmp(b)};fn partial(a:PartialOrd,b:PartialOrd)->?Ordering{a.partial_cmp(b)};fn main(){println(total(P{number:40},P{number:42}));println(partial(P{number:40},P{number:42}))}",
    );
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let artifact = compiled.into_artifact().unwrap();
    let restored = nsbc_io::read_artifact(&nsbc_io::write_artifact(&artifact).unwrap()).unwrap();
    assert_eq!(
        restored.type_pool.trait_schemas_snapshot(),
        artifact.type_pool.trait_schemas_snapshot()
    );
    for (view, method_name, parent, optional) in [
        (
            restored.type_pool.well_known.ord,
            "cmp",
            restored.type_pool.well_known.eq,
            false,
        ),
        (
            restored.type_pool.well_known.partial_ord,
            "partial_cmp",
            restored.type_pool.well_known.partial_eq,
            true,
        ),
    ] {
        let TypeKind::Trait { parents, .. } = &restored.type_pool.get(view).kind else {
            panic!("ordering view must be a trait")
        };
        assert!(parents.contains(&parent));
        let schema = restored.type_pool.trait_schema(view).unwrap();
        assert_eq!(str_interner::get(schema.slots[0].name), "eq");
        assert_eq!(schema.slots[0].trait_owner, parent);
        if !optional {
            assert_eq!(
                schema
                    .slots
                    .iter()
                    .map(|slot| str_interner::get(slot.name))
                    .collect::<Vec<_>>(),
                ["eq", "cmp", "lt", "gt", "lte", "gte"]
            );
        }

        let key = schema
            .slots
            .iter()
            .find(|key| str_interner::get(key.name) == method_name)
            .unwrap();
        let declaration = key.signature.as_ref().unwrap().declaration;
        let TypeKind::Function { ret, .. } = restored.type_pool.get(declaration).kind else {
            panic!("ordering contract must be a function")
        };
        let result = if optional {
            let TypeKind::Optional { inner } = restored.type_pool.get(ret).kind else {
                panic!("partial ordering must return Optional")
            };
            inner
        } else {
            ret
        };
        let TypeKind::Enum { name, variants } = &restored.type_pool.get(result).kind else {
            panic!("ordering result must retain nominal enum identity")
        };
        assert_eq!(str_interner::get(*name), "Ordering");
        assert_eq!(
            variants
                .iter()
                .map(|variant| str_interner::get(variant.name))
                .collect::<Vec<_>>(),
            ["less", "equal", "greater"]
        );
        for table in restored
            .type_pool
            .vtables_snapshot()
            .iter()
            .filter(|table| table.trait_type == view)
        {
            let slot = schema
                .slots
                .iter()
                .position(|slot| slot.name == key.name)
                .unwrap();
            let function = restored
                .codegen_output
                .functions
                .iter()
                .find(|function| function.func_id.0 == table.entries[slot])
                .unwrap();
            restored
                .type_pool
                .check_trait_method_signature(key, table.implementor, function.function_type)
                .unwrap();
            let original = artifact
                .codegen_output
                .functions
                .iter()
                .find(|original| original.func_id == function.func_id)
                .unwrap();
            assert_eq!(function.function_type, original.function_type);
            assert_eq!(function.abi, original.abi);
            assert_eq!(
                function.abi.as_ref().unwrap().physical_parameter_count(),
                function.param_count as usize
            );
            if !optional {
                for helper in &schema.slots[2..] {
                    let slot = schema
                        .slots
                        .iter()
                        .position(|slot| slot.name == helper.name)
                        .unwrap();
                    let function = restored
                        .codegen_output
                        .functions
                        .iter()
                        .find(|function| function.func_id.0 == table.entries[slot])
                        .unwrap();
                    restored
                        .type_pool
                        .check_trait_method_signature(
                            helper,
                            table.implementor,
                            function.function_type,
                        )
                        .unwrap();
                    let TypeKind::Struct { name, .. } =
                        &restored.type_pool.get(table.implementor).kind
                    else {
                        continue;
                    };
                    if str_interner::get(*name) == "P" {
                        assert_eq!(
                            function.abi.as_ref().unwrap().parameters,
                            [
                                nsbc::ParameterAbi::TraitSelf { view },
                                nsbc::ParameterAbi::TraitSelf { view }
                            ]
                        );
                        assert_eq!(function.param_count, 4);
                    }
                }
            }
        }
    }
}

#[test]
fn archive_ordinary_ord_defaults_preserve_overrides_and_frozen_scoped_proofs() {
    roundtrip(
        r#"
        struct P {};
        derive Eq for P;
        impl Ord for P {
            pub fn cmp(self,other:Self)->Ordering {Ordering.less};
            pub fn lt(self,other:Self)->bool {false}
        };
        fn overridden(a:Ord,b:Ord)->bool {not a.lt(b) and a.gte(b)};
        struct Derived {number:i64};
        derive Eq,Ord for Derived;
        fn derived(a:Ord,b:Ord)->bool {a.lt(b) and a.lte(b) and not a.gt(b) and not a.gte(b)};
        struct Scoped {};
        derive Eq for Scoped;
        mod a {
            extend Ord for Scoped {pub fn cmp(self,other:Self)->Ordering {Ordering.less}};
            pub fn answer()->bool {outside.compare(Scoped{},Scoped{})}
        };
        mod outside {
            extend Ord for Scoped {pub fn cmp(self,other:Self)->Ordering {Ordering.greater}};
            pub fn compare(left:Ord,right:Ord)->bool {left.lt(right)};
            pub fn own()->bool {compare(Scoped{},Scoped{})}
        };
        fn main() {
            println(overridden(P{},P{}));
            println(not(P{}.lt(P{})));
            println(P{}.gte(P{}));
            println(derived(Derived{number:40},Derived{number:42}));
            println(a.answer());
            println(not outside.own())
        }
        "#,
        "true\ntrue\ntrue\ntrue\ntrue\ntrue\n",
    );
}

#[test]
fn ordering_archive_rejects_runtime_result_that_disagrees_with_signed_contract() {
    for source in [
        "struct P{};derive Eq for P;impl Ord for P{pub fn cmp(self,other:Self)->Ordering{Ordering.less}};fn compare(a:Ord,b:Ord)->bool{a<b};fn main(){println(compare(P{},P{}))}",
        "struct P{};derive Eq for P;impl Ord for P{pub fn cmp(self,other:Self)->Ordering{Ordering.less}};struct Outer{value:P};derive Eq,Ord for Outer;fn main(){println(Outer{value:P{}}<Outer{value:P{}})}",
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
        let mut artifact = compiled.into_artifact().unwrap();
        let owner=artifact.type_pool.snapshot().types.iter().enumerate().find_map(|(index,info)| {
            matches!(&info.kind,type_pool::TypeKind::Struct{name,..} if str_interner::get(*name)=="P")
                .then_some(type_pool::TypeIndex::from_raw(index as u32))
        }).unwrap();
        let target = artifact
            .type_pool
            .find_trait_method(
                owner,
                artifact.type_pool.well_known.ord,
                str_interner::intern("cmp"),
            )
            .unwrap()
            .func_id;
        let function = artifact
            .codegen_output
            .functions
            .iter_mut()
            .find(|function| function.func_id.0 == target)
            .unwrap();
        let mut replaced = 0;
        for word in &mut function.instructions {
            if nsbc::Instruction::decode(*word).unwrap().opcode == nsbc::Opcode::Return {
                *word = nsbc::Instruction::return_unit().encode();
                replaced += 1;
            }
        }
        assert!(
            replaced > 0,
            "the genuine comparison body must contain a return"
        );
        let directory = TestDirectory::new();
        let path = directory.0.join("bad-runtime-ordering.nsbc");
        std::fs::write(&path, nsbc_io::write_artifact(&artifact).unwrap()).unwrap();
        let output = cli("run", &path);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("TypeError"));
    }
}

#[test]
fn archive_preserves_nested_character_enum_and_tuple_patterns() {
    roundtrip(
        "enum E{pair(a:char,b:(char,i64)),none};fn main(){print(E.pair('界',('🦀',42)) match {E.pair('界',('🦀',n))=>n,_=>0})}",
        "42",
    );
}

#[test]
fn archive_preserves_concrete_associated_returns_defaults_and_explicit_any() {
    roundtrip(
        r#"
        struct P {text:String};
        typealias Text=String;
        trait Stream {assoc Item:Type=Any;assoc typealias Output=?Item;fn next(self)->Output;fn pair(self,value:Item,raw:Any)->(Item,Any)};
        impl Stream for P {
            assoc Item:Type=Text;
            pub fn next(self)->?String {self.text};
            pub fn pair(self,value:String,raw:Any)->(String,Any) {(value,raw)}
        };
        struct Default {};
        trait NumberSource {assoc Item:Type=i64;fn number(self)->?Item};
        impl NumberSource for Default {pub fn number(self)->?i64 {42}};
        struct Raw {};
        trait RawSource {assoc Item:Type=Any;fn raw(self)->?Item};
        impl RawSource for Raw {pub fn raw(self)->?Any {42}};
        fn main() {
            let p=P{text:"abcd"++"efgh"};
            println(p.next().as(String).len()+34);
            let pair:(String,Any)=p.pair("abcdefgh",34);
            println(pair.0.len()+pair.1.as(i64));
            println(Default{}.number().as(i64));
            println(Raw{}.raw().as(i64))
        }
        "#,
        "42\n42\n42\n42\n",
    );
}

#[test]
fn archive_preserves_owned_and_scoped_associated_type_bindings() {
    let source = r#"
        struct P {};
        trait Stream {assoc Item:Type=Any;fn next(self)->Item};
        mod a {
            extend Stream for P {assoc Item:Type=String;pub fn next(self)->String {"abcdefgh"}};
            pub fn answer()->i64 {let p=P{};let text:String=p.next();text.len()+34}
        };
        mod b {
            extend Stream for P {assoc Item:Type=i64;pub fn next(self)->i64 {42}};
            pub fn answer()->i64 {let p=P{};let number:i64=p.next();number}
        };
        struct Both {};
        trait NumberSource {assoc Item:Type=Any;fn number(self)->Item};
        trait TextSource {assoc Item:Type=Any;fn text(self)->Item};
        impl NumberSource for Both {assoc Item:Type=i64;pub fn number(self)->i64 {42}};
        impl TextSource for Both {assoc Item:Type=String;pub fn text(self)->String {"abcdefgh"}};
        fn main() {println(a.answer());println(b.answer());let both=Both{};println(both.number());println(both.text().len()+34)}
        "#;
    roundtrip(source, "42\n42\n42\n42\n");
    let compiled = driver::Driver::new().compile(source);
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let artifact = compiled.into_artifact().unwrap();
    let restored = nsbc_io::read_artifact(&nsbc_io::write_artifact(&artifact).unwrap()).unwrap();
    assert_eq!(
        restored.type_pool.associated_bindings_snapshot(),
        artifact.type_pool.associated_bindings_snapshot()
    );
    assert_eq!(
        restored.type_pool.trait_schemas_snapshot(),
        artifact.type_pool.trait_schemas_snapshot()
    );
    for (index, table) in restored.type_pool.vtables_snapshot().iter().enumerate() {
        for descriptor in restored
            .type_pool
            .checked_vtable_descriptors(index)
            .unwrap()
        {
            let Some(signature) = &descriptor.signature else {
                continue;
            };
            if signature.associated_paths.is_empty() {
                continue;
            }
            let target = restored
                .codegen_output
                .functions
                .iter()
                .find(|function| function.func_id.0 == descriptor.func_id)
                .unwrap();
            let key = type_pool::TraitMethodKey {
                trait_owner: descriptor.trait_owner,
                name: descriptor.name,
                signature: descriptor.signature.clone(),
            };
            restored
                .type_pool
                .check_trait_method_signature_in_impl(
                    &key,
                    table.implementor,
                    target.function_type,
                    descriptor.implementation_trait,
                    descriptor.implementation_scope,
                )
                .unwrap();
        }
    }
}

#[test]
fn archive_preserves_associated_heap_returns_in_multishot_continuations() {
    roundtrip(
        r#"
        effect pause(catch k)->i64;
        struct P {text:String};
        trait Stream {assoc Item:Type=Any;fn next(self)->?Item};
        impl Stream for P {assoc Item:Type=String;pub fn next(self)->?String {pause()#;self.text}};
        fn main() {
            let p=P{text:"abcd"++"efgh"};
            let saved=p.next()#{pause(k)=>k};
            let first:String=saved(0).as(String);let second:String=saved(1).as(String);
            println(first.len()+34);println(second.len()+34)
        }
        "#,
        "42\n42\n",
    );
}

#[test]
fn invalid_associated_bindings_and_dynamic_views_do_not_produce_archives() {
    for source in [
        "struct P{};trait Stream{assoc Item:Type=Any;fn next(self)->Item};impl Stream for P{assoc Missing:Type=i64;pub fn next(self)->i64{42}};fn main(){}",
        "struct P{};trait Stream{assoc Item:Type=Any;fn next(self)->Item};impl Stream for P{assoc Item:Type=i64;assoc Item:Type=String;pub fn next(self)->i64{42}};fn main(){}",
        "struct P{};trait Stream{assoc Item:Type=Any;fn next(self)->Item};impl Stream for P{assoc Item:Type=42;pub fn next(self)->i64{42}};fn main(){}",
        "struct P{};trait Stream{assoc Item:Type=Any;fn next(self)->Item};impl Stream for P{assoc Item:Type=i64;pub fn next(self)->String{\"wrong\"}};fn main(){}",
        "trait Stream{assoc Item:Type=Any;fn next(self)->?Item};fn consume(x:Stream){x.next()};fn main(){}",
        "trait Stream{assoc Item:Type=Any;fn next(self)->?Item};fn consume(x:?Stream){};fn main(){}",
    ] {
        let directory = TestDirectory::new();
        let path = directory.0.join("bad-association.ns");
        std::fs::write(&path, source).unwrap();
        let output = cli("build", &path);
        assert!(!output.status.success(), "accepted {source}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("associated")
                || error.contains("binding")
                || error.contains("signature")
                || error.contains("trait parameter")
                || error.contains("duplicate"),
            "{error}"
        );
        assert!(!path.with_extension("nsbc").exists());
    }
}

#[test]
fn associated_archive_rejects_function_and_binding_contract_tampering() {
    let compiled=driver::Driver::new().compile(
        "struct P{};trait Stream{assoc Item:Type=Any;fn next(self)->?Item};impl Stream for P{assoc Item:Type=String;pub fn next(self)->?String{\"abcdefgh\"}};impl P{pub fn bad(self)->?i64{42}};fn main(){println(P{}.next().as(String))}",
    );
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let artifact = compiled.into_artifact().unwrap();
    let bytes = nsbc_io::write_artifact(&artifact).unwrap();
    let binding_index = artifact.type_pool.associated_bindings_snapshot().iter().position(|binding| matches!(artifact.type_pool.get(binding.trait_owner).kind, type_pool::TypeKind::Trait { name, .. } if str_interner::get(name)=="Stream")).unwrap();
    let binding = &artifact.type_pool.associated_bindings_snapshot()[binding_index];
    let target = artifact
        .type_pool
        .find_trait_method(
            binding.implementor,
            binding.trait_type,
            str_interner::intern("next"),
        )
        .unwrap()
        .func_id;
    let bad_target = artifact
        .type_pool
        .methods_of(binding.implementor)
        .iter()
        .find(|method| str_interner::get(method.name) == "bad")
        .unwrap()
        .func_id;
    let wrong_type = artifact
        .codegen_output
        .functions
        .iter()
        .find(|function| function.func_id.0 == bad_target)
        .unwrap()
        .function_type;
    let mut bad_function = nsbc_io::read_artifact(&bytes).unwrap();
    bad_function
        .codegen_output
        .functions
        .iter_mut()
        .find(|function| function.func_id.0 == target)
        .unwrap()
        .function_type = wrong_type;
    let error = nsbc_io::write_artifact(&bad_function)
        .unwrap_err()
        .to_string();
    assert!(error.contains("signature"), "{error}");
    let mut bad_binding = nsbc_io::read_artifact(&bytes).unwrap();
    let mut snapshot = bad_binding.type_pool.snapshot();
    snapshot.associated_bindings[binding_index].value = type_pool::Intrinsic::I64.type_index();
    bad_binding.type_pool = type_pool::TypePool::restore(snapshot).unwrap();
    let error = nsbc_io::write_artifact(&bad_binding)
        .unwrap_err()
        .to_string();
    assert!(error.contains("signature"), "{error}");
}

#[test]
fn archive_inherits_associated_binding_from_the_exact_forward_parent_record() {
    let source = r#"
        trait Base {assoc Item:Type=Any;fn next(self)->Item};
        trait Child(Base) {fn count(self)->i64};
        struct P {};
        impl Child for P {pub fn count(self)->i64 {42}};
        impl Base for P {assoc Item:Type=String;pub fn next(self)->String {"text"}};
        fn main() {let p=P{};println(p.count());println(p.next().len()+38)}
        "#;
    roundtrip(source, "42\n42\n");
    let compiled = driver::Driver::new().compile(source);
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let artifact = compiled.into_artifact().unwrap();
    let restored = nsbc_io::read_artifact(&nsbc_io::write_artifact(&artifact).unwrap()).unwrap();
    let bindings: Vec<_> = restored
        .type_pool
        .associated_bindings_snapshot()
        .iter()
        .filter(|binding| {
            ![
                restored.type_pool.well_known.iterator,
                restored.type_pool.well_known.into_iterator,
            ]
            .contains(&binding.trait_owner)
        })
        .collect();
    assert_eq!(bindings.len(), 2);
    assert_eq!(bindings[0].implementor, bindings[1].implementor);
    assert_eq!(bindings[0].trait_owner, bindings[1].trait_owner);
    assert_eq!(bindings[0].name, bindings[1].name);
    assert_ne!(bindings[0].trait_type, bindings[1].trait_type);
    for binding in bindings {
        assert_eq!(
            restored.type_pool.as_intrinsic(binding.value),
            Some(type_pool::Intrinsic::Str)
        );
        assert_eq!(str_interner::get(binding.name), "Item");
    }
}

#[test]
fn archive_executes_derived_enum_and_tuple_field_display_in_source_order() {
    roundtrip(
        r#"
        global calls:String="";
        struct Item{value:i64};
        impl Display for Item {
            pub fn to_string(self)->String {
                calls=calls++self.value.to_string();
                "item"++self.value.to_string()
            }
        };
        enum E{none,pair(first:Item,second:Item,text:String),optional(value:?Item),tuple(value:(Item,String,i64))};
        derive Display for E;
        typealias Pair=(Item,String);
        derive Display for Pair;
        typealias Single=(String,);
        derive Display for Single;
        struct Quoted{};
        impl Display for Quoted{pub fn to_string(self)->String{"\"custom\""}};
        typealias Custom=(Quoted,String);
        derive Display for Custom;
        fn show(x:Display)->String{x.to_string()};
        fn main(){
            println(show(E.none));
            println(show(E.pair(Item{value:40},Item{value:2},"你好")));
            println(show((Item{value:7},"quoted")));
            println(show(("single",)));
            println(show(E.optional(null)));
            println(show(E.optional(Item{value:3})));
            println(show((Quoted{},"raw")));
            println(show(E.tuple((Item{value:5},"inside",42))));
            println(calls);
            println((42,"legacy"));
            println(E.pair(Item{value:40},Item{value:2},"你好").to_string())
        }
        "#,
        "E.none\nE.pair(item40, item2, \"你好\")\n(item7, \"quoted\")\n(\"single\",)\nE.optional(null)\nE.optional(item3)\n(\"custom\", \"raw\")\nE.tuple((item5, \"inside\", 42))\n402735\n(42, \"legacy\")\nE.pair(item40, item2, \"你好\")\n",
    );
}

#[test]
fn archive_preserves_derived_display_field_calls_in_multishot_effects() {
    roundtrip(
        r#"
        effect pause(catch k)->i64;
        struct Item{text:String};
        impl Display for Item {
            pub fn to_string(self)->String {let n=pause()#;self.text++n.to_string()}
        };
        enum E{box(value:Item,text:String)};
        derive Display for E;
        fn show(x:Display)->String{x.to_string()};
        fn main(){
            let saved=show(E.box(Item{text:"abcd"++"efgh"},"tail"))# {pause(k)=>k};
            println(saved(40));
            println(saved(2))
        }
        "#,
        "E.box(abcdefgh40, \"tail\")\nE.box(abcdefgh2, \"tail\")\n",
    );
}

#[test]
fn unsupported_derived_display_fields_do_not_produce_archives() {
    for source in [
        "struct Hidden{}\nenum E{box(value:Hidden)}\nderive Display for E\nfn main(){}",
        "struct Hidden{}\ntypealias T=(Hidden,i64)\nderive Display for T\nfn main(){}",
        "struct Hidden{}\nmod scoped{extend Display for Hidden{pub fn to_string(self)->String{\"private evidence\"}}}\nenum E{box(value:Hidden)}\nderive Display for E\nfn main(){}",
    ] {
        let directory = TestDirectory::new();
        let path = directory.0.join("unsupported-display.ns");
        std::fs::write(&path, source).unwrap();
        let output = cli("build", &path);
        assert!(!output.status.success(), "{source}");
        assert!(!path.with_extension("nsbc").exists());
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        assert!(
            diagnostic.contains("Display") || diagnostic.contains("display"),
            "{diagnostic}"
        );
    }
}

#[test]
fn archive_keeps_legacy_struct_display_for_any_list_and_user_display_fields() {
    let source = r#"
        global calls:i64=0;
        struct Item{};
        impl Display for Item{pub fn to_string(self)->String{calls+=1;"changed"}};
        struct Raw{value:Any};derive Display for Raw;
        struct Bag{items:List,item:Item};derive Display for Bag;
        fn show(x:Display)->String{x.to_string()};
        fn main(){
            println(show(Raw{value:42}));
            let ignored=show(Bag{items:[40,2],item:Item{}});
            if calls==0 {println(42)} else {println(0)}
        }
        "#;
    roundtrip(source, "Raw { value: 42 }\n42\n");
    let compiled = driver::Driver::new().compile(source);
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let artifact = compiled.into_artifact().unwrap();
    assert!(
        artifact
            .builtins
            .iter()
            .any(|import| import.id == 120 && import.name == "__derived_display")
    );
    assert!(
        artifact
            .codegen_output
            .functions
            .iter()
            .all(|function| function.display_owner.is_none())
    );
    let loaded = nsbc_io::read_artifact(&nsbc_io::write_artifact(&artifact).unwrap()).unwrap();
    assert!(
        loaded
            .codegen_output
            .functions
            .iter()
            .all(|function| function.display_owner.is_none())
    );
}

#[test]
fn generated_enum_and_tuple_display_archives_require_exact_quote_native_capability() {
    let source = "enum E{box(value:(String,i64))}\nderive Display for E\nfn main(){println(E.box((\"你好\",42)).to_string())}";
    let compiled = driver::Driver::new().compile(source);
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let artifact = compiled.into_artifact().unwrap();
    assert_eq!(artifact.builtin_abi_version, 8);
    assert!(
        artifact
            .builtins
            .iter()
            .any(|import| import.id == 123 && import.name == "__display_quote")
    );
    assert!(
        artifact
            .codegen_output
            .functions
            .iter()
            .any(|function| function.display_owner.is_some())
    );
    let bytes = nsbc_io::write_artifact(&artifact).unwrap();
    let mut missing = nsbc_io::read_artifact(&bytes).unwrap();
    missing.builtins.retain(|import| import.id != 123);
    assert!(
        nsbc_io::write_artifact(&missing)
            .unwrap_err()
            .to_string()
            .contains("manifest")
    );
    for wrong_name in [false, true] {
        let mut bad = nsbc_io::read_artifact(&bytes).unwrap();
        if wrong_name {
            bad.builtins
                .iter_mut()
                .find(|import| import.id == 123)
                .unwrap()
                .name = "forged_quote".into();
        } else {
            bad.builtin_abi_version = 6;
        }
        let directory = TestDirectory::new();
        let path = directory.0.join("bad-quote.nsbc");
        std::fs::write(&path, nsbc_io::write_artifact(&bad).unwrap()).unwrap();
        let output = cli("run", &path);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("incompatible builtin ABI"));
    }
    // Build the legacy capability set directly; today's bundled std also
    // contains new Map helpers even when the user program does not call them.
    use nsbc::{Instruction as I, Reg};
    use type_pool::{FieldInfo, Intrinsic, TypeId, TypeInfo, TypeKind};
    let mut pool = type_pool::TypePool::with_intrinsics();
    let view = pool.well_known.display;
    let declaration = pool.intern_structural(TypeKind::Function {
        params: vec![view],
        ret: Intrinsic::Str.type_index(),
    });
    let name = str_interner::intern("to_string");
    pool.register_trait_schema(type_pool::TraitDispatchSchema {
        trait_type: view,
        slots: vec![type_pool::TraitMethodKey {
            trait_owner: view,
            name,
            signature: Some(type_pool::TraitMethodSignature {
                declaration,
                self_paths: vec![vec![type_pool::TraitTypeStep::Parameter(0)]],
                associated_paths: vec![],
                parameter_kinds: vec![type_pool::TraitParameterKind::Receiver],
            }),
        }],
    })
    .unwrap();
    let raw = pool.register(TypeInfo {
        kind: TypeKind::Struct {
            name: str_interner::intern("Raw"),
            fields: vec![FieldInfo {
                name: str_interner::intern("value"),
                ty: Intrinsic::Any.type_index(),
                has_default: false,
                offset: 0,
            }],
        },
        type_id: TypeId(943, 1),
        size: 8,
        align: 8,
    });
    let method = type_pool::MethodSlot {
        name,
        func_id: 1,
        trait_impl: Some(view),
        visible_scope: None,
        access: type_pool::MethodAccess::Public,
    };
    pool.add_method(raw, method.clone());
    pool.add_trait_impl(type_pool::TraitImplRecord {
        implementor: raw,
        trait_type: view,
        visible_scope: None,
        methods: vec![method],
    });
    pool.add_vtable(type_pool::VTable {
        implementor: raw,
        trait_type: view,
        visible_scope: None,
        entries: vec![1],
    });
    let mut wrapper = legacy_iterator_function(
        &mut pool,
        1,
        "legacy_struct_display",
        vec![raw],
        Intrinsic::Str.type_index(),
        vec![I::call_builtin(120, 1), I::ret(Reg(0))],
    );
    wrapper.abi = Some(nsbc::FunctionAbi {
        captures: vec![],
        parameters: vec![nsbc::ParameterAbi::Value],
    });
    let mut old = legacy_archive_fixture(
        pool,
        vec![
            I::new_object(Reg(0), raw),
            I::load_imm(Reg(1), 42),
            I::store_field(Reg(0), 0, Reg(1)),
            I::call(1, 1),
            I::call_builtin(1, 1),
            I::return_unit(),
        ],
        vec![],
        vec![
            nsbc::BuiltinImport {
                id: 120,
                name: "__derived_display".into(),
            },
            nsbc::BuiltinImport {
                id: 1,
                name: "println".into(),
            },
        ],
        6,
    );
    old.codegen_output.functions.push(wrapper);
    assert!(old.builtins.iter().all(|import| import.id < 123));
    let directory = TestDirectory::new();
    let path = directory.0.join("legacy-display.nsbc");
    std::fs::write(&path, nsbc_io::write_artifact(&old).unwrap()).unwrap();
    let output = cli("run", &path);
    assert_success(&output);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "Raw { value: 42 }\n"
    );
}

#[test]
fn derived_display_archives_enforce_shared_depth_and_size_budgets() {
    for (source, error) in [
        (
            "enum Chain{end,next(value:Chain)}\nderive Display for Chain\nfn main(){var value:Chain=Chain.end;var n:i64=0;while n<129{value=Chain.next(value);n+=1};println(value.to_string())}",
            "DisplayDepthExceeded",
        ),
        (
            "enum E{box(text:String)}\nderive Display for E\nfn main(){var text:String=\"\\x01\";var n:i64=0;while n<18{text=text++text;n+=1};println(E.box(text).to_string())}",
            "DisplaySizeExceeded",
        ),
    ] {
        let directory = TestDirectory::new();
        let path = directory.0.join("budget.ns");
        std::fs::write(&path, source).unwrap();
        let output = cli("run", &path);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(error),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_success(&cli("build", &path));
        let archive = path.with_extension("nsbc");
        std::fs::remove_file(path).unwrap();
        let output = cli("run", &archive);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(error),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn derived_display_archives_check_actual_user_field_method_returns() {
    let compiled=driver::Driver::new().compile("struct Item{}\nimpl Display for Item{pub fn to_string(self)->String{\"checked\"}}\nenum E{box(value:Item)}\nderive Display for E\nfn main(){println(E.box(Item{}).to_string())}");
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let mut artifact = compiled.into_artifact().unwrap();
    let owner = (0..artifact.type_pool.len())
        .find_map(|raw| {
            let index = type_pool::TypeIndex::from_raw(raw as u32);
            match &artifact.type_pool.get(index).kind {
                type_pool::TypeKind::Struct { name, .. }
                    if str_interner::try_get(*name).as_deref() == Some("Item") =>
                {
                    Some(index)
                }
                _ => None,
            }
        })
        .unwrap();
    let implementation = artifact
        .type_pool
        .find_trait_impl(owner, artifact.type_pool.well_known.display)
        .unwrap();
    let target = implementation
        .methods
        .iter()
        .find(|method| str_interner::try_get(method.name).as_deref() == Some("to_string"))
        .unwrap()
        .func_id;
    let function = artifact
        .codegen_output
        .functions
        .iter_mut()
        .find(|function| function.func_id.0 == target)
        .unwrap();
    function.instructions = vec![nsbc::Instruction::return_unit().encode()];
    function.safepoint_pcs.clear();
    artifact
        .codegen_output
        .method_call_scopes
        .as_mut()
        .unwrap()
        .retain(|context| context.func_id.0 != target);
    let directory = TestDirectory::new();
    let path = directory.0.join("bad-return.nsbc");
    std::fs::write(&path, nsbc_io::write_artifact(&artifact).unwrap()).unwrap();
    let output = cli("run", &path);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("TypeError"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn archive_specializes_associated_self_defaults_for_each_concrete_implementation() {
    let source = r#"
        trait Copy{assoc Item:Type=Self;fn copy(self)->Item};
        struct P{value:i64};struct Q{value:i64};
        impl Copy for P{pub fn copy(self)->Self{self}};
        impl Copy for Q{pub fn copy(self)->Self{self}};
        fn main(){
            let p:P=P{value:40}.copy();let q:Q=Q{value:2}.copy();
            println(p.value+q.value);
            println(type_of(p)==P);println(type_of(q)==Q)
        }
        "#;
    roundtrip(source, "42\ntrue\ntrue\n");
    let compiled = driver::Driver::new().compile(source);
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let artifact = compiled.into_artifact().unwrap();
    let loaded = nsbc_io::read_artifact(&nsbc_io::write_artifact(&artifact).unwrap()).unwrap();
    assert_eq!(
        loaded.type_pool.associated_defaults_snapshot(),
        artifact.type_pool.associated_defaults_snapshot()
    );
    let bindings: Vec<_> = loaded
        .type_pool
        .associated_bindings_snapshot()
        .iter()
        .filter(|binding| {
            ![
                loaded.type_pool.well_known.iterator,
                loaded.type_pool.well_known.into_iterator,
            ]
            .contains(&binding.trait_owner)
        })
        .collect();
    assert_eq!(bindings.len(), 2);
    for binding in bindings {
        assert_eq!(binding.value, binding.implementor);
        assert!(!loaded.type_pool.contains_associated_type(binding.value));
    }
    assert!(
        loaded
            .type_pool
            .associated_defaults_snapshot()
            .iter()
            .any(|default| matches!(
                default.expression,
                type_pool::AssociatedTypeExpr::SelfType { .. }
            ))
    );
}

#[test]
fn archive_applies_associated_dependency_defaults_after_explicit_overrides() {
    roundtrip(
        r#"
        trait Source{assoc Maybe:Type=?Item;assoc Item:Type=i64;fn maybe(self)->Maybe};
        struct Text{value:String};struct Number{value:i64};
        impl Source for Text{assoc Item:Type=String;pub fn maybe(self)->?String{self.value}};
        impl Source for Number{pub fn maybe(self)->?i64{self.value}};
        fn main(){
            let text:?String=Text{value:"abcdefgh"}.maybe();
            let number:?i64=Number{value:42}.maybe();
            println(text.as(String).len()+34);println(number.as(i64));
            println(type_of(text)==String)
        }
        "#,
        "42\n42\ntrue\n",
    );
}

#[test]
fn archive_preserves_forward_alias_and_nested_function_associated_defaults() {
    roundtrip(
        r#"
        trait Factory{
            assoc typealias Wrapped=(Item,Self);
            assoc Pair:Type=Wrapped;
            assoc Callback:Type=fn(Item)->?Self;
            assoc Item:Type=i64;
            fn pair(self)->Pair;
            fn callback(self)->Callback
        };
        struct P{text:String};
        impl Factory for P{
            assoc Item:Type=String;
            pub fn pair(self)->(String,Self){(self.text,self)};
            pub fn callback(self)->fn(String)->?Self{
                fn inner(value:String)->?P{null};inner
            }
        };
        fn main(){
            let p=P{text:"abcdefgh"};
            let pair:(String,P)=p.pair();println(pair.0.len()+34);
            let callback:fn(String)->?P=p.callback();
            let copied:?P=callback("argument");if copied==null{println(42)}else{println(0)};
            println(type_of(callback)==(fn(String)->?P))
        }
        "#,
        "42\n42\ntrue\n",
    );
}

#[test]
fn archive_specializes_associated_defaults_from_each_scoped_parent_provider() {
    let source = r#"
        struct P{};
        trait Base{assoc Item:Type=Self;fn item(self)->Item};
        trait Child(Base){assoc Pair:Type=(Item,i64);fn pair(self)->Pair};
        mod a{
            extend Base for P{assoc Item:Type=String;pub fn item(self)->String{"abcdefgh"}};
            extend Child for P{
                pub fn item(self)->String{"abcdefgh"};
                pub fn pair(self)->(String,i64){("abcdefgh",34)}
            };
            pub fn answer()->i64{let result:(String,i64)=P{}.pair();result.0.len()+result.1}
        };
        mod b{
            extend Base for P{assoc Item:Type=i64;pub fn item(self)->i64{40}};
            extend Child for P{
                pub fn item(self)->i64{40};pub fn pair(self)->(i64,i64){(40,2)}
            };
            pub fn answer()->i64{let result:(i64,i64)=P{}.pair();result.0+result.1}
        };
        fn main(){println(a.answer());println(b.answer())}
        "#;
    roundtrip(source, "42\n42\n");
    let compiled = driver::Driver::new().compile(source);
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let artifact = compiled.into_artifact().unwrap();
    let restored = nsbc_io::read_artifact(&nsbc_io::write_artifact(&artifact).unwrap()).unwrap();
    assert_eq!(
        restored.type_pool.associated_bindings_snapshot(),
        artifact.type_pool.associated_bindings_snapshot()
    );
    assert_eq!(
        restored.type_pool.associated_defaults_snapshot(),
        artifact.type_pool.associated_defaults_snapshot()
    );
    let item: Vec<_> = restored
        .type_pool
        .associated_bindings_snapshot()
        .iter()
        .filter(|binding| {
            str_interner::get(binding.name) == "Item"
                && binding.trait_owner != restored.type_pool.well_known.iterator
        })
        .collect();
    assert_eq!(item.len(), 4);
    assert!(item.iter().all(|binding| binding.visible_scope.is_some()));
    assert_eq!(
        item.iter()
            .filter(|binding| restored.type_pool.as_intrinsic(binding.value)
                == Some(type_pool::Intrinsic::Str))
            .count(),
        2
    );
    assert_eq!(
        item.iter()
            .filter(|binding| restored.type_pool.as_intrinsic(binding.value)
                == Some(type_pool::Intrinsic::I64))
            .count(),
        2
    );
}

#[test]
fn archive_preserves_uninstalled_associated_cycles_and_explicit_cycle_breaks() {
    for source in [
        "trait Source{assoc A:Type=B;assoc B:Type=A}\nfn main(){println(42)}",
        "struct P{}\ntrait Source{assoc A:Type=B;assoc B:Type=A;fn value(self)->A}\nimpl Source for P{assoc B:Type=i64;pub fn value(self)->i64{42}}\nfn main(){println(P{}.value())}",
    ] {
        roundtrip(source, "42\n");
    }
}

#[test]
fn invalid_associated_default_cycles_and_abstract_values_emit_no_archives() {
    for source in [
        "struct P{}\ntrait Source{assoc A:Type=B;assoc B:Type=A;fn value(self)->A}\nimpl Source for P{pub fn value(self)->i64{42}}\nfn main(){42}",
        "trait Source{assoc Item:Type=Source}\nfn main(){42}",
        "trait Source{assoc Item:Type=Self}\nfn main(){println(Source.Item)}",
        "trait Source{assoc Item:Type=Self}\nfn escape()->Source.Item{null}\nfn main(){42}",
    ] {
        let directory = TestDirectory::new();
        let path = directory.0.join("bad-default.ns");
        std::fs::write(&path, source).unwrap();
        let output = cli("build", &path);
        assert!(!output.status.success(), "{source}");
        assert!(!path.with_extension("nsbc").exists());
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        assert!(
            diagnostic.contains("associated") || diagnostic.contains("abstract"),
            "{diagnostic}"
        );
    }
}

#[test]
fn archive_keeps_specialized_associated_self_in_closures_and_multishot_frames() {
    roundtrip(
        "struct P{text:String}\ntrait Source{assoc Item:Type=Self;assoc Callback:Type=fn()->Item;fn callback(self)->Callback}\nimpl Source for P{pub fn callback(self)->fn()->P{||self}}\nfn main(){let f:fn()->P=P{text:\"abcd\"++\"efgh\"}.callback();println(f().text.len()+34);println(type_of(f)==(fn()->P))}",
        "42\ntrue\n",
    );
    roundtrip(
        "struct P{text:String}\neffect pause(catch k)->i64\ntrait Source{assoc Item:Type=Self;assoc Pair:Type=(Item,?Item);fn pair(self)->Pair}\nimpl Source for P{pub fn pair(self)->(P,?P){pause()#;(self,self)}}\nfn main(){let saved=P{text:\"abcd\"++\"efgh\"}.pair()#{pause(k)=>k};let first:(P,?P)=saved(0).as((P,?P));let second:(P,?P)=saved(1).as((P,?P));println(first.0.text.len()+first.1.as(P).text.len()+second.0.text.len()+second.1.as(P).text.len()+10)}",
        "42\n",
    );
}

#[test]
fn archive_specializes_associated_default_bodies_without_changing_explicit_any() {
    let source = r#"
        struct P{value:i64};struct Q{value:i64};
        trait Read{
            assoc Item:Type=Self;
            derive fn copy(self,other:Item)->Item{other};
            derive fn self_copy(self,other:Self)->Self{other};
            derive fn callback(self)->fn()->Item{||self};
            derive fn named(self)->fn(Item)->Item{fn identity(other:Item)->Item{other};identity};
            derive fn own_type(self)->Type{Item};
            derive fn cast(self,value:Any)->Item{value.as(Item)};
            derive fn explicit(self,value:Any)->Any{value}
        };
        impl Read for P{};impl Read for Q{};
        trait RawRead{
            assoc Item:Type=Any;
            derive fn exact(self,value:Item)->Item{value};
            derive fn unconverted(self,value:Any)->Any{value}
        };
        impl RawRead for P{assoc Item:Type=i64};
        impl RawRead for Q{assoc Item:Type=String};
        fn main(){
            let p=P{value:40};let q=Q{value:2};
            println(p.copy(p).value+q.copy(q).value);
            println(p.self_copy(p).value+q.self_copy(q).value);
            let pc=p.callback();let qc=q.callback();
            println(type_of(pc)==(fn()->P));println(type_of(qc)==(fn()->Q));
            println(pc().value+qc().value);
            let pn=p.named();let qn=q.named();
            println(type_of(pn)==(fn(P)->P));println(type_of(qn)==(fn(Q)->Q));
            println(pn(p).value+qn(q).value);
            println(p.own_type()==P);println(q.own_type()==Q);
            println(p.cast(p).value+q.cast(q).value);
            println(type_of(p.explicit(q))==Q);println(p.exact(42));println(type_of(p.unconverted(q))==Q)
        }
        "#;
    roundtrip(
        source,
        "42\n42\ntrue\ntrue\n42\ntrue\ntrue\n42\ntrue\ntrue\n42\ntrue\n42\ntrue\n",
    );
    let compiled = driver::Driver::new().compile(source);
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let artifact = compiled.into_artifact().unwrap();
    let loaded = nsbc_io::read_artifact(&nsbc_io::write_artifact(&artifact).unwrap()).unwrap();
    let records = loaded.type_pool.snapshot().trait_impls;
    for method in ["copy", "self_copy", "explicit", "unconverted"] {
        let implementations: Vec<_> = records
            .iter()
            .filter(|record| {
                record
                    .methods
                    .iter()
                    .any(|slot| str_interner::get(slot.name) == method)
            })
            .collect();
        assert_eq!(implementations.len(), 2);
        for implementation in implementations {
            let slot = implementation
                .methods
                .iter()
                .find(|slot| str_interner::get(slot.name) == method)
                .unwrap();
            let function = loaded
                .codegen_output
                .functions
                .iter()
                .find(|function| function.func_id.0 == slot.func_id)
                .unwrap();
            let abi = function.abi.as_ref().unwrap();
            assert!(
                matches!(abi.parameters[0],nsbc::ParameterAbi::TraitSelf{view} if view==implementation.trait_type)
            );
            assert!(abi.captures.is_empty());
            assert_eq!(
                function.param_count,
                if method == "self_copy" { 4 } else { 3 }
            );
            assert_eq!(
                abi.parameters[1],
                if method == "self_copy" {
                    nsbc::ParameterAbi::TraitSelf {
                        view: implementation.trait_type,
                    }
                } else {
                    nsbc::ParameterAbi::Value
                }
            );
            let type_pool::TypeKind::Function { params, ret } =
                &loaded.type_pool.get(function.function_type).kind
            else {
                panic!("expected specialized adapter function signature")
            };
            assert_eq!(params[0], implementation.implementor);
            if matches!(method, "explicit" | "unconverted") {
                assert_eq!(
                    (params[1], *ret),
                    (
                        type_pool::Intrinsic::Any.type_index(),
                        type_pool::Intrinsic::Any.type_index()
                    )
                );
            } else {
                assert_eq!(
                    (params[1], *ret),
                    (implementation.implementor, implementation.implementor)
                );
            }
        }
    }
}

#[test]
fn archive_keeps_associated_default_bodies_bound_to_their_scoped_implementation() {
    let source = r#"
        struct P{};
        trait Read{
            assoc Item:Type=i64;fn value(self)->Item;
            derive fn copy(self)->Item{self.value()};
            derive fn callback(self)->fn()->Item{||self.value()};
            derive fn own_type(self)->Type{Item}
        };
        mod a{
            extend Read for P{assoc Item:Type=String;pub fn value(self)->String{"abcdefgh"}};
            pub fn answer()->i64{P{}.copy().len()+34};
            pub fn callback()->fn()->String{P{}.callback()};
            pub fn correct_type()->bool{P{}.own_type()==String}
        };
        mod b{
            extend Read for P{pub fn value(self)->i64{42}};
            pub fn answer()->i64{P{}.copy()};
            pub fn callback()->fn()->i64{P{}.callback()};
            pub fn correct_type()->bool{P{}.own_type()==i64}
        };
        fn main(){
            println(a.answer());println(b.answer());
            let fa=a.callback();let fb=b.callback();
            println(fa().len()+34);println(fb());
            println(type_of(fa)==(fn()->String));println(type_of(fb)==(fn()->i64));
            println(a.correct_type());println(b.correct_type())
        }
        "#;
    roundtrip(source, "42\n42\n42\n42\ntrue\ntrue\ntrue\ntrue\n");
    let compiled = driver::Driver::new().compile(source);
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let artifact = compiled.into_artifact().unwrap();
    let loaded = nsbc_io::read_artifact(&nsbc_io::write_artifact(&artifact).unwrap()).unwrap();
    let mut targets = std::collections::BTreeSet::new();
    let mut returns = std::collections::BTreeSet::new();
    for implementation in loaded.type_pool.snapshot().trait_impls {
        let Some(slot) = implementation
            .methods
            .iter()
            .find(|method| str_interner::get(method.name) == "copy")
        else {
            continue;
        };
        assert!(implementation.visible_scope.is_some());
        assert!(targets.insert(slot.func_id));
        let function = loaded
            .codegen_output
            .functions
            .iter()
            .find(|function| function.func_id.0 == slot.func_id)
            .unwrap();
        let type_pool::TypeKind::Function { ret, .. } =
            loaded.type_pool.get(function.function_type).kind
        else {
            panic!("expected concrete adapter function")
        };
        returns.insert(ret.as_u32());
        assert_eq!(function.param_count, 2);
        assert_eq!(
            function.abi.as_ref().unwrap().parameters,
            vec![nsbc::ParameterAbi::TraitSelf {
                view: implementation.trait_type
            }]
        );
    }
    assert_eq!(targets.len(), 2);
    assert_eq!(
        returns,
        std::collections::BTreeSet::from([
            type_pool::Intrinsic::Str.type_index().as_u32(),
            type_pool::Intrinsic::I64.type_index().as_u32()
        ])
    );
}

#[test]
fn archive_prioritizes_user_overrides_of_associated_default_bodies() {
    roundtrip(
        "struct P{};trait Stream{assoc Item:Type=Any;fn next(self)->Item;derive fn copy(self)->Item{self.next()}};impl Stream for P{assoc Item:Type=i64;pub fn next(self)->i64{42}};fn main(){println(P{}.copy())}",
        "42\n",
    );
    roundtrip(
        r#"
        trait Read{assoc Item:Type=i64;fn value(self)->Item;derive fn copy(self)->Item{self.value()+1}};
        struct Override{};struct Default{};
        impl Read for Override{pub fn value(self)->i64{0};pub fn copy(self)->i64{42}};
        impl Read for Default{pub fn value(self)->i64{41}};
        fn main(){println(Override{}.copy());println(Default{}.copy())}
        "#,
        "42\n42\n",
    );
}

#[test]
fn associated_default_body_archives_reject_wrong_actual_returns_and_casts() {
    for expression in ["P{}.wrong(Q{})", "P{}.cast(Q{})", "P{}.callback(Q{})()"] {
        let source = format!(
            r#"
            struct P{{}};struct Q{{}};
            trait Read{{
                assoc Item:Type=Self;
                derive fn wrong(self,value:Any)->Item{{value}};
                derive fn cast(self,value:Any)->Item{{value.as(Item)}};
                derive fn callback(self,value:Any)->fn()->Item{{||value}}
            }};
            impl Read for P{{}};impl Read for Q{{}};
            fn main(){{println({expression})}}
            "#
        );
        let directory = TestDirectory::new();
        let path = directory.0.join("bad-return.ns");
        std::fs::write(&path, source).unwrap();
        let output = cli("run", &path);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("TypeError"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_success(&cli("build", &path));
        std::fs::remove_file(&path).unwrap();
        let output = cli("run", &path.with_extension("nsbc"));
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("TypeError"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn archive_keeps_associated_default_body_receivers_alive_in_closures_and_multishot_effects() {
    roundtrip(
        "struct P{text:String}\ntrait Read{assoc Item:Type=String;fn value(self)->Item;derive fn callback(self)->fn()->Item{||self.value()}}\nimpl Read for P{pub fn value(self)->String{self.text}}\nfn main(){let f=P{text:\"abcd\"++\"efgh\"}.callback();println(f().len()+34);println(type_of(f)==(fn()->String))}",
        "42\ntrue\n",
    );
    roundtrip(
        "effect pause(catch k)->i64\nstruct P{text:String}\ntrait Read{assoc Item:Type=String;fn value(self)->Item;derive fn copy(self)->Item{pause()#;self.value()}}\nimpl Read for P{pub fn value(self)->String{self.text}}\nfn main(){let saved=P{text:\"abcd\"++\"efgh\"}.copy()#{pause(k)=>k};let first:String=saved(0).as(String);let second:String=saved(1).as(String);println(first.len()+34);println(second.len()+34)}",
        "42\n42\n",
    );
}

#[test]
fn archive_preserves_inherited_associated_default_body_override_evidence() {
    roundtrip(
        "struct P{}\ntrait Base{assoc Item:Type=i64;fn value(self)->Item;derive fn copy(self)->Item{self.value()}}\ntrait Child(Base){derive fn answer(self)->i64{self.copy()+2}}\nimpl Base for P{pub fn value(self)->i64{2}}\nimpl Child for P{pub fn value(self)->i64{40}}\nfn main(){println(P{}.answer())}",
        "42\n",
    );
}

#[test]
fn archive_specializes_associated_default_body_field_layouts_for_each_implementor() {
    roundtrip(
        "struct P{pad:i64,value:i64}\nstruct Q{value:i64}\ntrait Project{assoc Item:Type=Self;fn item(self)->Item;derive fn answer(self)->i64{self.item().value}}\nimpl Project for P{pub fn item(self)->P{self}}\nimpl Project for Q{pub fn item(self)->Q{self}}\nfn main(){println(P{pad:99,value:40}.answer()+Q{value:2}.answer())}",
        "42\n",
    );
}

#[test]
fn archive_orders_global_initialization_through_associated_default_body_dispatch() {
    roundtrip(
        r#"
        mod api{pub global answer:i64=P{}.copy()};
        mod storage{pub global value:i64=42};
        struct P{};
        trait Read{assoc Item:Type=i64;fn next(self)->Item;derive fn copy(self)->Item{self.next()}};
        impl Read for P{pub fn next(self)->i64{storage.value}};
        fn main(){println(api.answer)}
        "#,
        "42\n",
    );
}

fn legacy_iterator_function(
    pool: &mut type_pool::TypePool,
    id: u32,
    name: &str,
    params: Vec<type_pool::TypeIndex>,
    ret: type_pool::TypeIndex,
    code: Vec<nsbc::Instruction>,
) -> nsbc::CompiledFunction {
    use nsbc::{CompiledFunction, FuncId, Instruction as I};
    use type_pool::TypeKind;
    let signature = pool.intern_structural(TypeKind::Function { params, ret });
    CompiledFunction {
        display_owner: None,
        func_id: FuncId(id),
        name: str_interner::intern(name),
        instructions: code.into_iter().map(I::encode).collect(),
        register_count: 20,
        param_count: if id == 0 { 0 } else { 1 },
        is_closure: false,
        function_type: signature,
        abi: None,
        safepoint_pcs: vec![],
    }
}
#[test]
fn legacy_iterator_archive_keeps_two_call_protocol() {
    use nsbc::{CodegenOutput, CompiledArtifact, FuncId, Instruction as I, Opcode, Reg};
    use type_pool::{
        Intrinsic, MethodAccess, MethodSlot, TraitDispatchSchema, TraitImplRecord, TraitMethodKey,
        TypeId, TypeInfo, TypeKind, TypePool, VTable,
    };
    let mut pool = TypePool::with_intrinsics();
    let view = pool.well_known.iterator;
    let owner = pool.register(TypeInfo {
        kind: TypeKind::Struct {
            name: str_interner::intern("LegacyIterator"),
            fields: vec![type_pool::FieldInfo {
                name: str_interner::intern("state"),
                ty: Intrinsic::I64.type_index(),
                offset: 0,
                has_default: false,
            }],
        },
        type_id: TypeId::ZERO,
        size: 8,
        align: 8,
    });
    let has_next = str_interner::intern("has_next");
    let next = str_interner::intern("next");
    pool.register_trait_schema(TraitDispatchSchema {
        trait_type: view,
        slots: vec![
            TraitMethodKey {
                trait_owner: view,
                name: has_next,
                signature: None,
            },
            TraitMethodKey {
                trait_owner: view,
                name: next,
                signature: None,
            },
        ],
    })
    .unwrap();
    let methods = vec![(has_next, 1), (next, 2)]
        .into_iter()
        .map(|(name, func_id)| MethodSlot {
            name,
            func_id,
            access: MethodAccess::Public,
            trait_impl: Some(view),
            visible_scope: None,
        })
        .collect::<Vec<_>>();
    for slot in &methods {
        pool.add_method(owner, slot.clone());
    }
    pool.add_trait_impl(TraitImplRecord {
        trait_type: view,
        implementor: owner,
        visible_scope: None,
        methods,
    });
    pool.add_vtable(VTable {
        trait_type: view,
        implementor: owner,
        visible_scope: None,
        entries: vec![1, 2],
    });
    let functions = vec![
        legacy_iterator_function(
            &mut pool,
            0,
            "legacy_entry",
            vec![],
            Intrinsic::Unit.type_index(),
            vec![
                I::new_object(Reg(19), owner),
                I::load_imm(Reg(0), 0),
                I::store_field(Reg(19), 0, Reg(0)),
                I::call_method(Reg(19), has_next.as_u32(), 0),
                I::jmp_if_not(Reg(0), 4),
                I::call_method(Reg(19), next.as_u32(), 0),
                I::call_builtin(1, 1),
                I::jmp(-4),
                I::return_unit(),
            ],
        ),
        legacy_iterator_function(
            &mut pool,
            1,
            "legacy_has_next",
            vec![owner],
            Intrinsic::Bool.type_index(),
            vec![
                I::load_field(Reg(1), Reg(0), 0),
                I::load_imm(Reg(2), 0),
                I::r_type(Opcode::CmpEq, Reg(0), Reg(1), Reg(2)),
                I::ret(Reg(0)),
            ],
        ),
        legacy_iterator_function(
            &mut pool,
            2,
            "legacy_next",
            vec![owner],
            Intrinsic::I64.type_index(),
            vec![
                I::load_imm(Reg(1), 1),
                I::store_field(Reg(0), 0, Reg(1)),
                I::load_imm(Reg(0), 42),
                I::ret(Reg(0)),
            ],
        ),
    ];
    let artifact = CompiledArtifact {
        codegen_output: CodegenOutput {
            method_call_scopes: None,
            scope_coverage: nsbc::ScopeCoverage::Calls,
            functions,
            constants: vec![],
            globals: vec![],
        },
        type_pool: pool,
        entry: Some(FuncId(0)),
        builtin_abi_version: 1,
        builtins: vec![nsbc::BuiltinImport {
            id: 1,
            name: "println".into(),
        }],
    };
    let bytes = nsbc_io::write_artifact(&artifact).unwrap();
    let restored = nsbc_io::read_artifact(&bytes).unwrap();
    for view in [
        restored.type_pool.well_known.iterator,
        restored.type_pool.well_known.into_iterator,
    ] {
        let TypeKind::Trait { assoc_types, .. } = &restored.type_pool.get(view).kind else {
            panic!()
        };
        assert!(assoc_types.is_empty());
    }
    assert!(
        restored
            .type_pool
            .trait_schema(view)
            .unwrap()
            .slots
            .iter()
            .all(|key| key.signature.is_none())
    );
    assert!(restored.type_pool.associated_bindings_snapshot().is_empty());
    assert_eq!(nsbc_io::write_artifact(&restored).unwrap(), bytes);
    let directory = TestDirectory::new();
    let path = directory.0.join("legacy-iterator.nsbc");
    std::fs::write(&path, bytes).unwrap();
    let output = cli("run", &path);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "42\n");
}

#[test]
fn composite_patterns_preserve_binding_identity_after_source_deletion() {
    for source in [
        "enum E{left(value:i64),right(value:i64)};fn main(){let f=E.left(42) match {E.left(n) or E.right(n)=>||n};println(f())}",
        "enum E{left(value:i64),right(value:i64)};fn main(){let f=E.right(42) match {(E.left(n) or E.right(n)) as whole=>||if whole'type==E{n}else{0}};println(f())}",
        "enum E{left(value:i64),right(value:i64),none};fn main(){let total:i64=0;for (E.left(n) or E.right(n)) as whole if n>0 in [E.none,null,E.left(40),E.right(2),E.left(-1)]{if whole'type==E{total+=n}};println(total)}",
        "global count:i64=0;fn once()->(i64,i64){count+=1;(40,2)};fn main(){let result=once() match {(0,n) or (n,2)=>n+2,_=>0};println(result);println(count)}",
        "enum E{a(value:i64),b(value:i64)};fn main(){println(E.b(42) matches ((E.a(n) or E.b(n)) as whole if whole'type==E and n==42))}",
    ] {
        let expected = if source.contains("println(count)") {
            "42\n1\n"
        } else if source.contains("println(E.b") {
            "true\n"
        } else {
            "42\n"
        };
        roundtrip(source, expected);
    }
}

#[test]
fn invalid_composite_pattern_cli_inputs_never_create_archives() {
    for (source, diagnostic) in [
        (
            "enum E{a(value:i64),b(value:i64)};fn main(){E.a(42) match {E.a(x) or E.b(y)=>42}}",
            "same names",
        ),
        (
            "enum E{a(value:i64),b(value:String)};fn main(){E.a(42) match {E.a(x) or E.b(x)=>42}}",
            "same type",
        ),
        (
            "fn main(){42 match {42 as (x,y)=>42}}",
            "identifier binding",
        ),
        ("fn main(){let x as y=42;x}", "supported in match"),
    ] {
        let directory = TestDirectory::new();
        let path = directory.0.join("invalid-pattern.ns");
        std::fs::write(&path, source).unwrap();
        let output = cli("build", &path);
        assert!(!output.status.success(), "{source}");
        assert!(!path.with_extension("nsbc").exists());
        let message = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(message.contains(diagnostic), "{source}: {message}");
    }
}
