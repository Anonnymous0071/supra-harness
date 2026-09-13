wit_bindgen::generate!({
    path: "../../crates/supra_plugin/src/supra.wit",
    world: "agent",
});

struct ContractSmoke;

impl Guest for ContractSmoke {
    fn run(arguments: String) -> String {
        arguments
    }
}

export!(ContractSmoke);
