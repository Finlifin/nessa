//! Generated field calls retain the namespace load dependencies of their source targets.
mod common;

#[test]
fn generated_display_loads_the_field_implementation_namespace() {
    let source = r#"
        struct Item {}
        enum Box { item(value:Item) }
        derive Display for Box
        const rendered = Box.item(Item{}).to_string()
        mod Provider {
            var label:String = "before"
            fn __init__() { label = "ready" }
            impl Display for Item {
                fn to_string(self)->String { label }
            }
        }
        fn main() { if rendered == "Box.item(ready)" {42} else {0} }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}
