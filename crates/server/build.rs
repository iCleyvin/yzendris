fn main() {
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/icon.ico");
        res.set("CompanyName",      "cleyvinOS");
        res.set("FileDescription",  "Yzendris KVM Server");
        res.set("ProductName",      "Yzendris");
        res.set("LegalCopyright",   "cleyvinOS");
        res.compile().expect("failed to embed resources");
    }
}
