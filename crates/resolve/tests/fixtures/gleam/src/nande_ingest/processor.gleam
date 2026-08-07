import nande_ingest/scrub

pub fn process(data: String) -> String {
  scrub.payload(data)
}
