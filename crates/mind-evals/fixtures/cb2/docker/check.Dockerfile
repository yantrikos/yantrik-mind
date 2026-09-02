# E.CB2 checker image: fixed browser (Playwright 1.62.1, chromium) + Python 3 + pytest. Every
# artifact is executed ONLY inside this image, on a writable copy, under net/cb2net.sh.
FROM mcr.microsoft.com/playwright:v1.62.1-noble
RUN apt-get update && apt-get install -y --no-install-recommends python3 python3-pip && rm -rf /var/lib/apt/lists/* \
 && pip3 install --no-cache-dir --break-system-packages pytest==8.3.5
RUN mkdir -p /checker && cd /checker && npm init -y >/dev/null && npm install --no-audit --no-fund playwright@1.62.1 >/dev/null
COPY checks/check_web.mjs /checker/check_web.mjs
COPY checks/check_t3.py /checker/check_t3.py
COPY seed /checker/seed
WORKDIR /work
