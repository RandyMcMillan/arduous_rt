default:
  just --choose

help:
  @make help

build:
  @make build

test:
  @make test

test-captured:
  @cargo test

test-external:
  @make test-external

test-all:
  @make test-all

demos:
  @make demos

proofs:
  @make proofs

stress:
  @make stress

demo-backpressure:
  @make demo-backpressure

demo-panic:
  @make demo-panic

check:
  @make check

fmt:
  @make fmt

clippy:
  @make clippy

doc:
  @make doc

clean:
  @make clean
