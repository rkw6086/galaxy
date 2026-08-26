.PHONY: run clean deploy install

run:
	cargo run --bin galaxy-oss

clean:
	cargo clean

deploy:
	./script/build-and-upload-hermes.sh

install:
	./script/build-and-install-to-applications.sh
