fn main() {
    let unix = cfg!(unix);
    let libei = cfg!(feature = "libei");
    let x11 = cfg!(feature = "x11");
    let macos = cfg!(target_os = "macos");
    let wlroots = cfg!(feature = "wlroots");
    let rdp = cfg!(feature = "remote_desktop_portal");

    let libei = unix && !macos && libei;
    let wlroots = unix && !macos && wlroots;
    let x11 = unix && !macos && x11;
    let rdp = unix && !macos && rdp;

    println!("cargo::rustc-check-cfg=cfg(wlroots)");
    println!("cargo::rustc-check-cfg=cfg(libei)");
    println!("cargo::rustc-check-cfg=cfg(x11)");
    println!("cargo::rustc-check-cfg=cfg(rdp)");

    if libei {
        println!("cargo::rustc-cfg=libei");
    }
    if x11 {
        println!("cargo::rustc-cfg=x11");
    }
    if wlroots {
        println!("cargo::rustc-cfg=wlroots");
    }
    if rdp {
        println!("cargo::rustc-cfg=rdp");
    }
}
