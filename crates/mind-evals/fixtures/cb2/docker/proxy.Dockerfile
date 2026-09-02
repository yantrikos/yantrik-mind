# E.CB2 model proxy image: standard library only.
FROM python:3.13-slim
COPY proxy/proxy.py /proxy.py
RUN useradd -m -u 10002 cb2proxy && mkdir -p /count && chown cb2proxy /count
USER cb2proxy
CMD ["python3", "/proxy.py"]
