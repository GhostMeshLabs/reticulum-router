ENGINE=?podman
REPO=codeberg.io/kallisti5
TAG=latest

default:
	podman build . -t $(REPO)/reticulum-router:$(TAG)
test:
	podman run $(REPO)/reticulum-router:$(TAG)
push:
	podman push $(REPO)/reticulum-router:$(TAG)
