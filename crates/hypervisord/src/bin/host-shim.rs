fn main() {
    let mut args = std::env::args_os().skip(1);
    let (Some(mode), Some(metadata), Some(root), None) =
        (args.next(), args.next(), args.next(), args.next())
    else {
        std::process::exit(2);
    };
    let (metadata, root) = (std::path::Path::new(&metadata), std::path::Path::new(&root));
    let result = if mode == "shim" {
        hypervisord::driver::serve(metadata, root)
    } else if mode == "profile" {
        hypervisord::driver::seatbelt_profile(metadata, root).map(|text| print!("{text}"))
    } else {
        std::process::exit(2);
    };
    if let Err(error) = result {
        eprintln!("host shim: {error}");
        std::process::exit(1);
    }
}
