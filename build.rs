fn main() {
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=icon.ico");
        winresource::WindowsResource::new()
            .set_icon("icon.ico")
            .compile()
            .expect("failed to embed the Windows application icon");
    }
}
