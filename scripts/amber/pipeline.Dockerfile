# syntax=docker/dockerfile:1.7
# BuildKit stage for the vaked pipeline: install vakedc editable and run a
# check inside a container. The --mount=type=cache on pip's cache dir is the
# caching payoff - the wheel/download work persists across rebuilds instead of
# repeating every time.
#
# Build context is the vaked-base checkout (see scripts/amber/pipeline.ab).
FROM python:3.12-slim
WORKDIR /vaked
COPY . /vaked
RUN --mount=type=cache,target=/root/.cache/pip pip install --editable .
RUN vakedc check vaked/examples/crabcc-umami.vaked
