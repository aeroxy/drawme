var http = require('http');
var fs = require('fs');
var path = require('path');
var url = require('url');
var io = require('socket.io');
var mime = require('mime');
var httpServer = http.createServer(function(req,res) {
	var cacheLoad;
	if(req.url == "/" || req.url == "") {
		cacheLoad = "./www/index.html";
	}
	else {
		cacheLoad = "./www" + url.parse(req.url).pathname;
	}
	var httpStatusCode = 200;
	fs.exists(cacheLoad,function(statusCheck) {
		if(!statusCheck) {
			httpStatusCode = 404;
			cacheLoad = "./www/index.html";
		}
		var cache = fs.readFileSync(cacheLoad);
		var mimeType = mime.lookup(cacheLoad);
		res.writeHead(httpStatusCode,{'Content-type':mimeType});
		res.end(cache);
	});
})
httpServer.listen(8000);
var webSocket = io.listen(httpServer);
webSocket.sockets.on('connection',function(socket){
	socket.on('start',function(x,y) {
		socket.broadcast.emit('start',x,y);
	});
	socket.on('move',function(x,y) {
		socket.broadcast.emit('move',x,y);
	});
	socket.on('stop',function() {
		socket.broadcast.emit('stop');
	});
	socket.on('clear',function() {
		socket.broadcast.emit('clear');
	});
});