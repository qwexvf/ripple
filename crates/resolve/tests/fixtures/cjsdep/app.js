const { useQuery } = require('urql');
const { render: renderIt } = require('react-dom');
const lodash = require('lodash');

function app() {
  return useQuery() + renderIt() + lodash.map();
}
module.exports = { app };
