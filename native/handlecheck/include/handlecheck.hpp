#pragma once

#include <map>
#include <string>
#include <vector>

namespace hc {

enum class State { Found, NotFound, Blocked, RateLimited, Unknown, Error, Invalid };
enum class PayloadKind { Object, ExactArray, NullableObject };

struct Site {
    std::string id;
    std::string name;
    std::string request_template;
    std::string profile_template;
    std::string username_pattern;
    std::string identity_pointer;
    std::vector<std::string> required_pointers;
    PayloadKind payload_kind = PayloadKind::Object;
    bool case_sensitive = false;
    bool json_404_is_not_found = true;
    std::string example_username;
    std::string docs_url;
};

struct HttpResponse {
    long status = 0;
    std::string body;
    std::map<std::string, std::string> headers;
    std::string effective_url;
    std::string error;
    long elapsed_ms = 0;
};

struct Evidence {
    std::string field;
    std::string expected;
    std::string actual;
};

struct Result {
    std::string site_id;
    std::string site_name;
    std::string username;
    std::string profile_url;
    std::string request_url;
    State state = State::Unknown;
    std::string reason;
    long http_status = 0;
    long elapsed_ms = 0;
    std::vector<Evidence> evidence;
    std::string control = "not_run";
    std::string checked_at;
};

struct HttpOptions { long timeout_ms = 10000; };

std::string state_name(State state);
std::string ascii_lower(std::string value);
std::string percent_encode(const std::string& value);
std::string expand_url(const std::string& pattern, const std::string& username);
bool valid_username(const Site& site, const std::string& username);
const std::vector<Site>& sites();
Result classify(const Site& site, const std::string& username, const HttpResponse& response);
bool http_global_init();
void http_global_cleanup();
HttpResponse fetch(const std::string& url, const HttpOptions& options);

} // namespace hc
