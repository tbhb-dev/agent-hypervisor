fn main() {
    let mut args = std::env::args_os().skip(1);
    let (Some(mode), Some(metadata), Some(root), None) =
        (args.next(), args.next(), args.next(), args.next())
    else {
        std::process::exit(2);
    };
    if mode != "shim" {
        std::process::exit(2);
    }
    if let Err(error) =
        hypervisord::driver::serve(std::path::Path::new(&metadata), std::path::Path::new(&root))
    {
        eprintln!("host shim: {error}");
        std::process::exit(1);
    }
}
