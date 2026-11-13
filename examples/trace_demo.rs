use arduous_rt::trace;

fn main() {
    trace!("trace demo starting");
    let rt = arduous_rt::global_rt();
    rt.block_on(async {
        trace!("inside runtime");
    });
    trace!("trace demo done");
}
