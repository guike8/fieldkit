#include "handlecheck.hpp"
#include <curl/curl.h>
#include <algorithm>
#include <chrono>
#include <cctype>
#include <climits>
#include <string>

namespace hc {
namespace {
constexpr size_t max_body_bytes = 1024 * 1024;
constexpr size_t max_header_bytes = 64 * 1024;
struct Buffer {
    HttpResponse response;
    size_t header_bytes = 0;
    bool oversized = false;
};

size_t receive_body(char* ptr, size_t size, size_t count, void* data) {
    auto& buffer = *static_cast<Buffer*>(data);
    if (size != 0 && count > SIZE_MAX / size) return 0;
    const size_t bytes = size * count;
    if (bytes > max_body_bytes - buffer.response.body.size()) {
        buffer.oversized = true;
        return 0;
    }
    try { buffer.response.body.append(ptr, bytes); }
    catch (...) { return 0; }
    return bytes;
}

std::string trim(std::string value) {
    const auto begin = value.find_first_not_of(" \t\r\n");
    if (begin == std::string::npos) return {};
    return value.substr(begin, value.find_last_not_of(" \t\r\n") - begin + 1);
}

size_t receive_header(char* ptr, size_t size, size_t count, void* data) {
    auto& buffer = *static_cast<Buffer*>(data);
    if (size != 0 && count > SIZE_MAX / size) return 0;
    const size_t bytes = size * count;
    if (bytes > max_header_bytes - buffer.header_bytes) {
        buffer.oversized = true;
        return 0;
    }
    buffer.header_bytes += bytes;
    try {
        std::string line(ptr, bytes);
        // Discard headers from an interim response or an HTTP CONNECT proxy.
        if (line.rfind("HTTP/", 0) == 0) buffer.response.headers.clear();
        const auto colon = line.find(':');
        if (colon != std::string::npos)
            buffer.response.headers[ascii_lower(trim(line.substr(0, colon)))] = trim(line.substr(colon + 1));
    } catch (...) { return 0; }
    return bytes;
}
} // namespace

bool http_global_init() { return curl_global_init(CURL_GLOBAL_DEFAULT) == CURLE_OK; }
void http_global_cleanup() { curl_global_cleanup(); }

HttpResponse fetch(const std::string& url, const HttpOptions& options) {
    Buffer buffer;
    buffer.response.effective_url = url;
    if (url.rfind("https://", 0) != 0) {
        buffer.response.error = "Only HTTPS endpoints are supported";
        return buffer.response;
    }
    CURL* curl = curl_easy_init();
    if (!curl) {
        buffer.response.error = "Could not initialize HTTP client";
        return buffer.response;
    }
    char error_buffer[CURL_ERROR_SIZE] = {};
    curl_slist* headers = curl_slist_append(nullptr, "Accept: application/json");
    curl_easy_setopt(curl, CURLOPT_URL, url.c_str());
    curl_easy_setopt(curl, CURLOPT_HTTPHEADER, headers);
    curl_easy_setopt(curl, CURLOPT_USERAGENT, "Handlecheck/0.1 (public-profile lookup)");
    curl_easy_setopt(curl, CURLOPT_FOLLOWLOCATION, 0L);
    curl_easy_setopt(curl, CURLOPT_NOSIGNAL, 1L);
    curl_easy_setopt(curl, CURLOPT_SSL_VERIFYPEER, 1L);
    curl_easy_setopt(curl, CURLOPT_SSL_VERIFYHOST, 2L);
#if LIBCURL_VERSION_NUM >= 0x075500
    curl_easy_setopt(curl, CURLOPT_PROTOCOLS_STR, "https");
#else
    curl_easy_setopt(curl, CURLOPT_PROTOCOLS, CURLPROTO_HTTPS);
#endif
    curl_easy_setopt(curl, CURLOPT_TIMEOUT_MS, options.timeout_ms);
    curl_easy_setopt(curl, CURLOPT_CONNECTTIMEOUT_MS, std::min(4000L, options.timeout_ms));
    curl_easy_setopt(curl, CURLOPT_MAXFILESIZE_LARGE, static_cast<curl_off_t>(max_body_bytes));
    curl_easy_setopt(curl, CURLOPT_ACCEPT_ENCODING, "");
    curl_easy_setopt(curl, CURLOPT_WRITEFUNCTION, receive_body);
    curl_easy_setopt(curl, CURLOPT_WRITEDATA, &buffer);
    curl_easy_setopt(curl, CURLOPT_HEADERFUNCTION, receive_header);
    curl_easy_setopt(curl, CURLOPT_HEADERDATA, &buffer);
    curl_easy_setopt(curl, CURLOPT_ERRORBUFFER, error_buffer);
    const auto begin = std::chrono::steady_clock::now();
    const CURLcode code = curl_easy_perform(curl);
    buffer.response.elapsed_ms = static_cast<long>(std::chrono::duration_cast<std::chrono::milliseconds>(
        std::chrono::steady_clock::now() - begin).count());
    curl_easy_getinfo(curl, CURLINFO_RESPONSE_CODE, &buffer.response.status);
    char* effective = nullptr;
    curl_easy_getinfo(curl, CURLINFO_EFFECTIVE_URL, &effective);
    if (effective) buffer.response.effective_url = effective;
    if (code != CURLE_OK) {
        buffer.response.error = buffer.oversized ? "Response exceeded size limit" :
            (error_buffer[0] ? error_buffer : curl_easy_strerror(code));
    }
    curl_slist_free_all(headers);
    curl_easy_cleanup(curl);
    return buffer.response;
}
} // namespace hc
