import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_4142 extends AI {
public AI_4142() throws LeekRunException {
super(1, 4);
}
public void staticInit() throws LeekRunException {
}
public Object runIA(Session session) throws LeekRunException {
ops(1); return xor(true, true);
}
protected String getAIString() { return "<snippet 4142>";}
protected String[] getErrorFiles() { return new String[] {"<snippet 4142>", };}

protected int[] getErrorFilesID() { return new int[] {4142, };}

}
