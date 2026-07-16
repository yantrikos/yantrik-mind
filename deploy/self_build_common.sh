#!/usr/bin/env bash

# Keep authentication/quota failures from consuming a queued self-build goal.
builder_unavailable() {
  grep -qiE "credit balance is too low|usage limit|quota exceeded|invalid api key|authentication_error|invalid authentication credentials|oauth token.*expired|401 unauthorized" <<<"$1"
}
