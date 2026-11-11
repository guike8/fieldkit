// Test-only transport. The production executable always links src/http.cpp.
#include "handlecheck.hpp"
#include <nlohmann/json.hpp>

#include <cstdlib>
#include <fstream>
#include <mutex>
#include <regex>

namespace hc {
namespace {
std::mutex trace_mutex;

std::string environment(const char* name) {
    const auto* value = std::getenv(name);
    return value ? value : "";
}

void trace(const std::string& entry) {
    const auto path = environment("HC_TEST_TRACE");
    if (path.empty()) return;
    std::lock_guard<std::mutex> lock(trace_mutex);
    std::ofstream output(path, std::ios::app);
    output << entry << '\n';
}
} // namespace

bool http_global_init() {
    trace("init");
    return true;
}

void http_global_cleanup() { trace("cleanup"); }

HttpResponse fetch(const std::string& url, const HttpOptions&) {
    trace("GET " + url);
    HttpResponse response;
    response.effective_url = url;
    response.elapsed_ms = 1;
    response.headers["content-type"] = "application/json";
    const auto scenario = environment("HC_TEST_SCENARIO");

    if (scenario == "network_error") {
        response.error = "fixture: DNS lookup failed";
        return response;
    }

    const auto slash = url.find_last_of('/');
    const auto username = slash == std::string::npos ? url : url.substr(slash + 1);
    const bool control = std::regex_match(username, std::regex("hc[0-9a-f]{12}"));
    response.status = 404;
    response.body = nlohmann::json({{"message", "Not Found"}}).dump();

    if (url.find("https://api.github.com/users/") != 0) return response;

    if (control && scenario == "control_blocked") {
        response.status = 403;
        response.body = nlohmann::json({{"message", "Forbidden"}}).dump();
        return response;
    }
    if (scenario == "doctor_missing_positive" && username == "octocat") return response;

    if ((!control && (ascii_lower(username) == "alice" || username == "octocat")) ||
        scenario == "control_false_positive") {
        response.status = 200;
        nlohmann::json profile = {{"login", username}, {"id", 42}, {"type", "User"}};
        if (scenario == "positive_missing_id") profile.erase("id");
        response.body = profile.dump();
    }
    return response;
}
} // namespace hc
