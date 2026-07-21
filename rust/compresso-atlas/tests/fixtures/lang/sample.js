function connect(url) {
  return fetch(url);
}

const retry = async (fn) => {
  return fn();
};

class Pool {
  acquire() {
    return {};
  }
}
