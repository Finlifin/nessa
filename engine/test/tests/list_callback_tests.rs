mod common;

#[test]
fn empty_lists_skip_all_callbacks_and_preserve_fold_initial_values() {
    let source = r#"
        global calls:i64=0
        fn main(){
            let xs=List()
            let mapped=xs.map(|x|{calls+=1;x})
            let filtered=xs.filter(|x|{calls+=1;true})
            xs.each(|x|{calls+=1})
            xs.foreach(|x|{calls+=1})
            let initial=[40]
            let folded=xs.fold(initial,|acc,x|{calls+=1;null}).as(List)
            folded.push(2)
            let nil=xs.fold(null,|acc,x|42)
            let unit=xs.fold((),|acc,x|42)
            if calls==0 and mapped.len()==0 and filtered.len()==0
                and initial.len()==2 and nil==null and unit==(){
                initial(0).as(i64)+initial(1).as(i64)
            }else{0}
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn callbacks_visit_each_input_once_in_order_including_rejected_elements() {
    for expression in [
        "[1,2,3].map(|x|{trace=trace*10+x.as(i64);calls+=1;x})",
        "[1,2,3].filter(|x|{trace=trace*10+x.as(i64);calls+=1;x.as(i64)==2})",
        "[1,2,3].fold(0,|acc,x|{trace=trace*10+x.as(i64);calls+=1;acc+x})",
        "[1,2,3].each(|x|{trace=trace*10+x.as(i64);calls+=1})",
        "[1,2,3].foreach(|x|{trace=trace*10+x.as(i64);calls+=1})",
    ] {
        let source = format!(
            "global trace:i64=0;global calls:i64=0;fn main(){{{expression};if trace==123 and calls==3{{42}}else{{0}}}}"
        );
        assert_eq!(common::run_value(&source).unwrap(), 42, "{expression}");
    }
}

#[test]
fn null_and_unit_are_real_callback_values_and_accumulators() {
    for source in [
        "fn main(){let xs=[1,2].map(|x|null);if xs.len()==2 and xs(0)==null and xs(1)==null{42}else{0}}",
        "fn main(){let xs=[1,2].map(|x|());if xs.len()==2 and xs(0)==() and xs(1)==(){42}else{0}}",
        "fn main(){let xs=[null,(),42].filter(|x|x==null or x==());if xs.len()==2 and xs(0)==null and xs(1)==(){42}else{0}}",
        "fn main(){let xs=[null,()];let result=xs.fold(42,|acc,x|x);if result==(){42}else{0}}",
        "fn main(){let result=[(),null].fold((),|acc,x|x);if result==null{42}else{0}}",
        "global trace:i64=0;fn main(){[null,()].each(|x|{if x==null{trace=trace*10+1}else{trace=trace*10+2}});if trace==12{42}else{0}}",
        "fn main(){let a=[null].each(|x|());let b=[()].foreach(|x|());if a==() and b==(){42}else{0}}",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

#[test]
fn every_method_uses_input_slots_from_before_source_pop_set_and_push() {
    for expression in [
        "let result=xs.map(|x|{xs.pop();xs.set(0,99);xs.push(100);trace=trace*10+x.as(i64);x});if result.len()!=3 or result(2)!=3{return 0}",
        "let result=xs.filter(|x|{xs.pop();xs.set(0,99);xs.push(100);trace=trace*10+x.as(i64);true});if result.len()!=3 or result(2)!=3{return 0}",
        "let result=xs.fold(0,|acc,x|{xs.pop();xs.set(0,99);xs.push(100);trace=trace*10+x.as(i64);acc*10+x});if result!=123{return 0}",
        "xs.each(|x|{xs.pop();xs.set(0,99);xs.push(100);trace=trace*10+x.as(i64)})",
        "xs.foreach(|x|{xs.pop();xs.set(0,99);xs.push(100);trace=trace*10+x.as(i64)})",
    ] {
        let source = format!(
            "global trace:i64=0;fn main(){{let xs=[1,2,3];{expression};if trace==123 and xs.len()==3 and xs(0)==99 and xs(2)==100{{42}}else{{0}}}}"
        );
        assert_eq!(common::run_value(&source).unwrap(), 42, "{expression}");
    }
}

#[test]
fn shallow_snapshots_and_outputs_share_referenced_element_objects() {
    for source in [
        "fn main(){let item=[0];let xs=[item,item];let result=xs.map(|x|{let inner=x.as(List);inner.push(inner.len());inner});result(0).as(List).push(42);if item.len()==4 and result(1).as(List).len()==4 and xs(0).as(List).len()==4{42}else{0}}",
        "fn main(){let item=[0];let result=[item,item].filter(|x|{x.as(List).push(1);true});result(0).as(List).push(42);if item.len()==4 and result(1).as(List).len()==4{42}else{0}}",
        "fn main(){let item=[0];let result=[item,item].fold(item,|acc,x|{acc.as(List).push(1);x}).as(List);result.push(42);if item.len()==4{42}else{0}}",
        "fn main(){let item=[0];[item,item].foreach(|x|{x.as(List).push(1)});if item.len()==3{42}else{0}}",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

#[test]
fn nested_and_chained_callbacks_keep_independent_traversals_and_captures() {
    for source in [
        "fn main(){let bias=1;let xs=[1,2,3].map(|x|[10,20].map(|y|x+y+bias).fold(0,|acc,y|acc+y));xs.filter(|x|x.as(i64)>34).fold(0,|acc,x|acc+x).as(i64)-32}",
        "fn main(){let xs=[1,2];let output=xs.map(|x|xs.fold(0,|acc,y|acc+x*y));if output(0)==3 and output(1)==6{42}else{0}}",
        "fn main(){let log=List();[1,2].each(|x|{[3,4].foreach(|y|{log.push(x*10+y)})});if log.len()==4 and log(0)==13 and log(1)==14 and log(2)==23 and log(3)==24{42}else{0}}",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

#[test]
fn returned_callbacks_keep_captured_heap_values_alive_after_creator_returns() {
    let source = r#"
        fn make()->fn(Any)->Any{
            let values=[40]
            let bias=1
            let callback=|x|{values.push(x);values(0)+x+bias}
            callback
        }
        fn main(){
            let callback=make()
            let first=[1,2].map(callback)
            let second=[3].map(callback)
            if first(0)==42 and first(1)==43 and second(0)==44{42}else{0}
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn callbacks_capture_primitive_values_but_share_captured_list_references() {
    let source = r#"
        fn main(){
            var local:i64=0
            let visited=List()
            [40,2].each(|x|{local+=x.as(i64);visited.push(x)})
            if local==0 and visited.len()==2{
                visited(0).as(i64)+visited(1).as(i64)
            }else{0}
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn named_and_any_erased_callbacks_work_for_filter_fold_and_visitors() {
    for source in [
        "fn positive(x:i64)->bool{x>0};fn main(){let f:Any=positive;[-1,40,2].filter(f).fold(0,|acc,x|acc+x).as(i64)}",
        "fn decimal(acc:i64,x:i64)->i64{acc*10+x};fn main(){let f:Any=decimal;[4,2].fold(0,f).as(i64)}",
        "global total:i64=0;fn record(x:i64)->Unit{total+=x};fn main(){let f:Any=record;[20].each(record);[22].foreach(f);total}",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

#[test]
fn missing_arguments_and_named_incompatible_callbacks_reject_artifacts() {
    for source in [
        "fn main(){List().map()}",
        "fn main(){List().filter()}",
        "fn main(){List().fold(0)}",
        "fn main(){List().each()}",
        "fn main(){List().foreach()}",
        "fn pred(x:Any)->Unit{};fn main(){[1].filter(pred)}",
        "fn visit(x:Any)->bool{true};fn main(){[1].foreach(visit)}",
        "fn combine(acc:Any,x:Any,extra:Any)->Any{acc};fn main(){[1].fold(0,combine)}",
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "accepted {source}");
        assert!(
            compiled.codegen_output.functions.is_empty(),
            "produced code for {source}"
        );
        assert!(
            compiled.into_artifact().is_err(),
            "produced artifact for {source}"
        );
    }
}

#[test]
fn any_erased_callbacks_reject_invalid_runtime_arguments_and_results() {
    for source in [
        "fn main(){let f:Any=42;[1].map(f)}",
        "fn main(){let f:Any=|x,y|x;[1].map(f)}",
        "fn decimal(acc:i64,x:i64)->i64{acc*10+x};fn main(){let f:Any=decimal;[1].fold(null,f)}",
        "fn pred(x:i64)->bool{true};fn main(){let f:Any=pred;[()].filter(f)}",
        "fn main(){let f:Any=|x|\"true\";[1].filter(f)}",
        "fn main(){let f:Any=|x|[];[1].each(f)}",
        "fn main(){let f:Any=|acc|acc;[1].fold(0,f)}",
    ] {
        let error = common::run_value(source).unwrap_err();
        assert!(error.contains("TypeError"), "{source}: {error}");
    }
}
