fn main() {
    println!("cargo:rerun-if-changed=assets/logo.ico");

    #[cfg(target_os = "windows")]
    {
        let mut resource = winres::WindowsResource::new();
        resource.set_icon("assets/logo.ico");
        if let Err(error) = resource.compile() {
            println!("cargo:warning=could not embed exe icon: {error}");
        }
    }
}
