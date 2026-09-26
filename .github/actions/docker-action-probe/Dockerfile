FROM alpine@sha256:ce64758a109eb420d874a118f87920e625e12d3634e03b4a5573fd9f6e5d3507

COPY pre.sh main.sh post.sh docker-probe-tool /probe/
RUN chmod 0755 /probe/pre.sh /probe/main.sh /probe/post.sh /probe/docker-probe-tool
