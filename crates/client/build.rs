// UIAccess manifest: lets the injected mouse/keyboard cross the UIPI integrity
// boundary so the client can drive UAC consent prompts (which run at System
// integrity) and the Secure Desktop — input from a non-UIAccess process, even
// an elevated one, is silently dropped by those windows.
//
// For Windows to actually GRANT uiAccess at launch, all of these must hold:
//   • requestedExecutionLevel uiAccess="true"   (this manifest)
//   • the .exe is Authenticode-signed by a cert chaining to a trusted root
//     installed in the machine's Trusted Root store
//   • the .exe lives in a secure path (%ProgramFiles% or %SystemRoot%\System32)
//   • UAC (EnableLUA) is on
// See scripts/install-windows-uiaccess.ps1 for the signing + install steps.
#[cfg(windows)]
const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="true" />
      </requestedPrivileges>
    </security>
  </trustInfo>
</assembly>
"#;

fn main() {
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/icon.ico");
        res.set("CompanyName",      "cleyvinOS");
        res.set("FileDescription",  "Yzendris KVM Client");
        res.set("ProductName",      "Yzendris");
        res.set("LegalCopyright",   "cleyvinOS");
        res.set_manifest(MANIFEST);
        res.compile().expect("failed to embed resources");
    }
}
