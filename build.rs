use shadow_rs::ShadowBuilder;

fn main() {
    ShadowBuilder::builder()
        .deny_const(Default::default())
        .build()
        .expect("shadow build");
}
