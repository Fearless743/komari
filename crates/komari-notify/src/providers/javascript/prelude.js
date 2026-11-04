// Web-style APIs injected into user scripts before the script itself runs.
//
// The heavy lifting (HTTP, timers, microtask driving) happens in Rust; this
// prelude only shapes the request/response objects so that scripts written for
// the Go (goja) runtime keep working. It mirrors
// `utils/messageSender/javascript/apis.go`.
(function () {
  "use strict";

  var bridge = globalThis.__komari_http;
  var bridgeSync = globalThis.__komari_http_sync;
  delete globalThis.__komari_http;
  delete globalThis.__komari_http_sync;

  function headersFrom(pairs) {
    var headers = {};
    for (var i = 0; i < pairs.length; i += 1) {
      headers[pairs[i][0]] = pairs[i][1];
    }
    Object.defineProperty(headers, "get", {
      enumerable: false,
      value: function get(name) {
        var wanted = String(name).toLowerCase();
        var keys = Object.keys(headers);
        for (var j = 0; j < keys.length; j += 1) {
          if (keys[j].toLowerCase() === wanted) {
            return headers[keys[j]];
          }
        }
        return null;
      },
    });
    return headers;
  }

  function responseFrom(raw) {
    var headers = headersFrom(raw.headers);
    return {
      status: raw.status,
      statusText: raw.statusText,
      ok: raw.ok,
      headers: headers,
      text: function text() {
        return Promise.resolve(raw.body);
      },
      json: function json() {
        return new Promise(function (resolve, reject) {
          try {
            resolve(JSON.parse(raw.body));
          } catch (error) {
            // Go rejects with a string as well.
            reject("Failed to parse JSON: " + (error && error.message ? error.message : String(error)));
          }
        });
      },
    };
  }

  function requestFrom(url, options) {
    options = options || {};
    var headers = {};
    if (options.headers) {
      var keys = Object.keys(options.headers);
      for (var i = 0; i < keys.length; i += 1) {
        headers[keys[i]] = String(options.headers[keys[i]]);
      }
    }
    var body = null;
    if (options.body !== undefined && options.body !== null) {
      body = String(options.body);
    }
    return {
      method:
        options.method === undefined || options.method === null
          ? "GET"
          : String(options.method),
      url: String(url),
      headers: headers,
      body: body,
    };
  }

  globalThis.fetch = function fetch(url, options) {
    if (arguments.length < 1) {
      throw new TypeError("fetch requires at least 1 argument");
    }
    return bridge(requestFrom(url, options)).then(responseFrom, function (reason) {
      // Go: reject(vm.ToValue(fmt.Sprintf("Fetch failed: %v", err))).
      return Promise.reject("Fetch failed: " + reason);
    });
  };

  function XMLHttpRequest() {
    this.readyState = 0;
    this.status = 0;
    this.statusText = "";
    this.responseText = "";
    this.response = "";
    this.onreadystatechange = null;
    this.onload = null;
    this.onerror = null;
    this._method = "GET";
    this._url = "";
    this._headers = {};
    this._async = true;
  }

  XMLHttpRequest.prototype._fire = function (name) {
    var handler = this[name];
    // Go passes the request object itself as the first argument.
    if (typeof handler === "function") {
      handler.call(this, this);
    }
  };

  XMLHttpRequest.prototype.open = function (method, url, async) {
    if (arguments.length < 2) {
      throw new TypeError("open requires at least 2 arguments");
    }
    this._method = String(method);
    this._url = String(url);
    this._async = async === undefined ? true : Boolean(async);
    this.readyState = 1;
    this._fire("onreadystatechange");
  };

  XMLHttpRequest.prototype.setRequestHeader = function (name, value) {
    if (arguments.length < 2) {
      throw new TypeError("setRequestHeader requires 2 arguments");
    }
    this._headers[String(name)] = String(value);
  };

  XMLHttpRequest.prototype.send = function (body) {
    var self = this;
    var request = requestFrom(this._url, {
      method: this._method,
      headers: this._headers,
      body: body,
    });

    function apply(raw) {
      self.readyState = 2;
      self._fire("onreadystatechange");
      self.readyState = 3;
      self._fire("onreadystatechange");
      self.readyState = 4;
      self.status = raw.status;
      self.statusText = raw.statusText;
      self.responseText = raw.body;
      self.response = raw.body;
      self._fire("onreadystatechange");
      self._fire("onload");
    }

    function fail(error) {
      self.readyState = 4;
      self.status = 0;
      self.statusText =
        typeof error === "string"
          ? error
          : error && error.message
            ? String(error.message)
            : "request failed";
      // Go calls onerror first, then onreadystatechange.
      self._fire("onerror");
      self._fire("onreadystatechange");
    }

    if (this._async === false) {
      try {
        apply(bridgeSync(request));
      } catch (error) {
        fail(error);
      }
      return;
    }
    bridge(request).then(apply, fail);
  };

  XMLHttpRequest.prototype.getAllResponseHeaders = function () {
    return "";
  };

  XMLHttpRequest.prototype.getResponseHeader = function () {
    return null;
  };

  globalThis.XMLHttpRequest = XMLHttpRequest;

  // goja_nodejs registers no native modules, so every `require` call fails.
  // Go's provider only enables the registry; it never registers a module.
  globalThis.require = function require() {
    throw new Error("Invalid module");
  };
})();
