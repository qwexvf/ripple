pub fn payload(data: String) -> String {
  case luhn(digits_only(data)) {
    True -> mask(data)
    False -> data
  }
}

fn luhn(digits: String) -> Bool {
  True
}

fn digits_only(s: String) -> String {
  s
}

fn mask(s: String) -> String {
  s
}
