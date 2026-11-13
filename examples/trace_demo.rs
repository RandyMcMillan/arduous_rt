use arduous_rt::arduous_trace;

fn main() {
    tracing_subscriber::fmt::init();
    arduous_trace!("trace demo starting");
    let rt = arduous_rt::global_rt();
    rt.block_on(async {
        arduous_trace!("inside runtime");
    });
    arduous_trace!("trace demo done");
}
